use std::sync::Arc;

use napi::bindgen_prelude::ClassInstance;
use napi_derive::napi;

use crate::error::{err, err_str, thread_local};
use crate::reference::{Reference, build_reference};
use crate::repository::Repository;
use crate::tree::{Tree, build_tree};

/// Options for constructing a [`Head`] via `new Head(options)`.
///
/// `repo` is optional: when omitted the resulting `Head` is a pure snapshot and
/// its resolution methods (`id`, `ref`, `tree`, …) will throw.
#[napi(object)]
pub struct HeadOptions {
    /// The repository this `HEAD` belongs to. Optional: without it the `Head`
    /// cannot resolve to a commit/tree/reference.
    pub repo: Option<ClassInstance<'static, Repository>>,
    /// The name of the reference `HEAD` points at (`refs/heads/...`), or `null`
    /// when `HEAD` is detached or unborn.
    pub referent: Option<String>,
    /// The kind of `HEAD`: `"symbolic"` (points at a branch), `"detached"`
    /// (points directly at an object), or `"unborn"` (points at a branch that
    /// does not exist yet).
    pub kind: String,
}

/// The repository HEAD, exposed to TypeScript.
#[napi]
pub struct Head {
    inner: Option<Arc<gix::ThreadSafeRepository>>,
    referent: Option<String>,
    kind: String,
}

#[napi]
impl Head {
    /// Construct a `Head` from [`HeadOptions`].
    #[napi(constructor)]
    pub fn new(options: HeadOptions) -> Self {
        Self {
            inner: options.repo.map(|r| r.inner.clone()),
            referent: options.referent,
            kind: options.kind,
        }
    }

    /// The name of the reference `HEAD` points at (`refs/heads/...`), or `null`
    /// when `HEAD` is detached or unborn.
    #[napi(getter, js_name = "referent")]
    pub fn referent(&self) -> Option<String> {
        self.referent.clone()
    }

    /// The kind of `HEAD`: `"symbolic"` (points at a branch), `"detached"`
    /// (points directly at an object), or `"unborn"` (points at a branch that
    /// does not exist yet).
    #[napi(getter)]
    pub fn kind(&self) -> String {
        self.kind.clone()
    }

    /// The object id `HEAD` points to.
    #[napi]
    pub fn id(&self) -> napi::Result<String> {
        let r = thread_local(self.require_inner()?);
        let id = r.head_id().map_err(|e| err("head_id", e))?;
        Ok(id.to_string())
    }

    /// The reference `HEAD` points to, or `null` if `HEAD` is detached or unborn.
    ///
    /// Exposed to JS as `ref` (`ref` is a Rust keyword, so it cannot be the Rust
    /// method name; the napi macro also rejects the `r#ref` raw identifier).
    #[napi(js_name = "ref")]
    pub fn head_ref(&self) -> napi::Result<Option<Reference>> {
        let r = thread_local(self.require_inner()?);
        match r.head_ref().map_err(|e| err("head_ref", e))? {
            Some(rf) => Ok(Some(build_reference(self.require_inner()?, rf)?)),
            None => Ok(None),
        }
    }

    /// The root tree object `HEAD` points to.
    #[napi]
    pub fn tree(&self) -> napi::Result<Tree> {
        let r = thread_local(self.require_inner()?);
        let tree = r.head_tree().map_err(|e| err("head_tree", e))?;
        build_tree(self.require_inner()?, tree.id)
    }

    /// The object id of the root tree pointed to by `HEAD`.
    #[napi]
    pub fn tree_id(&self) -> napi::Result<String> {
        let r = thread_local(self.require_inner()?);
        let id = r.head_tree_id().map_err(|e| err("head_tree_id", e))?;
        Ok(id.to_string())
    }

    /// The object id of the root tree pointed to by `HEAD`, falling back to the
    /// empty tree if `HEAD` is unborn.
    #[napi]
    pub fn tree_id_or_empty(&self) -> napi::Result<String> {
        let r = thread_local(self.require_inner()?);
        let id = r
            .head_tree_id_or_empty()
            .map_err(|e| err("head_tree_id_or_empty", e))?;
        Ok(id.to_string())
    }

    /// Whether `HEAD` is detached (points directly at an object id rather than a
    /// branch). Derivable from `kind`, so it works even without a repository.
    #[napi]
    pub fn detached(&self) -> napi::Result<bool> {
        Ok(self.kind == "detached")
    }
}

impl Head {
    fn require_inner(&self) -> napi::Result<&Arc<gix::ThreadSafeRepository>> {
        self.inner.as_ref().ok_or_else(|| {
            err_str(
                "head",
                "Head was constructed without a repository instance; cannot resolve",
            )
        })
    }
}

#[napi]
impl Repository {
    /// The repository `HEAD`.
    #[napi]
    pub fn head(&self) -> napi::Result<Head> {
        let r = thread_local(&self.inner);
        let head = r.head().map_err(|e| err("head", e))?;
        let referent = head.referent_name().map(|n| n.to_string());
        let kind = if head.is_detached() {
            "detached"
        } else if head.is_unborn() {
            "unborn"
        } else {
            "symbolic"
        };
        Ok(Head {
            inner: Some(self.inner.clone()),
            referent,
            kind: kind.to_string(),
        })
    }

    /// The name of the reference `HEAD` points to (e.g. `refs/heads/main`),
    /// or `null` when `HEAD` is detached or unborn.
    #[napi]
    pub fn head_name(&self) -> napi::Result<Option<String>> {
        Ok(self.head_ref()?.map(|r| r.name()))
    }

    /// Alias for [`Repository::head_name`].
    #[napi]
    pub fn head_path(&self) -> napi::Result<Option<String>> {
        self.head_name()
    }

    /// The object id `HEAD` points to.
    #[napi]
    pub fn head_id(&self) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        let id = r.head_id().map_err(|e| err("head_id", e))?;
        Ok(id.to_string())
    }

    /// The object id of the root tree pointed to by HEAD.
    #[napi]
    pub fn head_tree_id(&self) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        let id = r.head_tree_id().map_err(|e| err("head_tree_id", e))?;
        Ok(id.to_string())
    }

    /// The object id of the root tree pointed to by HEAD, falling back to the empty tree
    /// if HEAD is unborn.
    #[napi]
    pub fn head_tree_id_or_empty(&self) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        let id = r
            .head_tree_id_or_empty()
            .map_err(|e| err("head_tree_id_or_empty", e))?;
        Ok(id.to_string())
    }

    /// The reference HEAD points to, or null if HEAD is detached or unborn.
    #[napi]
    pub fn head_ref(&self) -> napi::Result<Option<Reference>> {
        let r = thread_local(&self.inner);
        match r.head_ref().map_err(|e| err("head_ref", e))? {
            Some(rf) => Ok(Some(build_reference(&self.inner, rf)?)),
            None => Ok(None),
        }
    }

    /// The root tree object HEAD points to.
    #[napi]
    pub fn head_tree(&self) -> napi::Result<Tree> {
        let r = thread_local(&self.inner);
        let tree = r.head_tree().map_err(|e| err("head_tree", e))?;
        build_tree(&self.inner, tree.id)
    }

    /// Whether HEAD is detached (points directly at an object id rather than a branch).
    /// Mirrors git2's `git_repository_head_detached`.
    #[napi]
    pub fn head_detached(&self) -> napi::Result<bool> {
        let r = thread_local(&self.inner);
        Ok(r.head().map_err(|e| err("head", e))?.is_detached())
    }
}
