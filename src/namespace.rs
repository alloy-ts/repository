use std::sync::Arc;

use napi_derive::napi;

use crate::error::{cached_namespace_get, cached_namespace_set, err, thread_local};
use crate::reference::Reference;
use crate::repository::Repository;

/// Canonicalise a raw namespace name into its `refs/namespaces/...` form.
///
/// Falls back to the raw name when it is not a valid namespace, so values that
/// were never validated (e.g. set by the environment) still display sensibly.
pub(crate) fn namespace_canonical(raw: &str) -> String {
    gix::refs::namespace::expand(raw)
        .map(|n| String::from_utf8_lossy(n.into_bstring().as_slice()).into_owned())
        .unwrap_or_else(|_| raw.to_string())
}

/// Expand a namespace *name* into the canonical prefix git uses, per the
/// [git namespace documentation](https://git-scm.com/docs/gitnamespaces):
/// `foo` becomes `refs/namespaces/foo/`, and `foo/bar` is treated as two nested
/// namespaces: `refs/namespaces/foo/refs/namespaces/bar/`.
#[napi]
pub fn namespace_expand(namespace: String) -> napi::Result<String> {
    gix::refs::namespace::expand(namespace.as_str())
        .map(|n| String::from_utf8_lossy(n.as_bstr()).into_owned())
        .map_err(|e| err("namespace_expand", e))
}

#[napi]
impl Repository {
    /// Return the currently set reference namespace in its canonical
    /// `refs/namespaces/<name>/` form, if any.
    ///
    /// The value is cached per repository because each synchronous call gets a fresh
    /// thread-local view whose in-memory namespace would not otherwise persist.
    #[napi]
    pub fn namespace(&self) -> Option<String> {
        cached_namespace_get(Arc::as_ptr(&self.inner) as usize).map(|raw| namespace_canonical(&raw))
    }

    /// Set the reference namespace and return the previously set one (if any), in
    /// canonical form.
    #[napi]
    pub fn set_namespace(&self, ns: String) -> napi::Result<Option<String>> {
        let key = Arc::as_ptr(&self.inner) as usize;
        let previous = cached_namespace_get(key).map(|raw| namespace_canonical(&raw));
        // Validate against gix (and derive the canonical form) before caching the
        // raw name, which `thread_local()` re-applies on every call.
        let _canonical = gix::refs::namespace::expand(ns.as_str())
            .map(|n| String::from_utf8_lossy(n.into_bstring().as_slice()).into_owned())
            .map_err(|e| err("namespace_expand", e))?;
        let mut r = thread_local(&self.inner);
        r.set_namespace(ns.as_str())
            .map_err(|e| err("set_namespace", e))?;
        cached_namespace_set(key, Some(ns));
        Ok(previous)
    }

    /// Clear the reference namespace and return the previously set one (if any), in
    /// canonical form.
    #[napi]
    pub fn clear_namespace(&self) -> napi::Result<Option<String>> {
        let key = Arc::as_ptr(&self.inner) as usize;
        let previous = cached_namespace_get(key).map(|raw| namespace_canonical(&raw));
        let mut r = thread_local(&self.inner);
        r.clear_namespace();
        cached_namespace_set(key, None);
        Ok(previous)
    }
}

#[napi]
impl Reference {
    /// Return the full validated name of this reference with the given reference
    /// namespace stripped, or `null` if the name isn't prefixed with it.
    ///
    /// The namespace is a namespace *name* like `foo`, expanded per-component to the
    /// canonical `refs/namespaces/foo/` prefix (mirroring `setNamespace`). Already
    /// expanded prefixes and arbitrary ref prefixes like `refs/heads/` are not
    /// namespace names and will not match.
    #[napi]
    pub fn name_without_namespace(&self, namespace: String) -> napi::Result<Option<String>> {
        let namespace = gix::refs::namespace::expand(namespace.as_str())
            .map_err(|e| err("namespace_expand", e))?;
        let full_name =
            gix::refs::FullName::try_from(self.name.as_str()).map_err(|e| err("ref_name", e))?;
        use gix::bstr::ByteSlice;
        Ok(full_name
            .as_bstr()
            .as_bytes()
            .strip_prefix(namespace.as_bstr().as_bytes())
            .map(|stripped| String::from_utf8_lossy(stripped).into_owned()))
    }
}
