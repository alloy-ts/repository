use std::sync::Arc;

use napi::Error as NapiError;

use gix::Error as GixError;

/// Upper bound on how many causes are attached to a thrown error, so pathological
/// source chains (or cycles) cannot blow up the FFI boundary.
const MAX_CAUSES: usize = 16;

/// Build a [`napi::Error`] from a domain label and a native error.
///
/// The message keeps the historical `"{what}: {e}"` shape, so existing consumers are unaffected.
/// The gix error tree is additionally attached as a standard JS `error.cause` chain: one
/// `Error` per cause, innermost last, skipping the root diagnostic (already in the message).
/// Causes carrying a gix [`gix::error::Class`]ification get a machine-readable
/// `[git: <Class>]` suffix (e.g. `[git: NotFound]`) appended to their message.
pub fn err(what: &str, e: impl std::error::Error + 'static) -> NapiError {
    let mut out = NapiError::from_reason(format!("{what}: {e}"));
    out.cause = build_cause(&e);
    out
}

/// Build a [`napi::Error`] from a plain message with no underlying native error.
pub fn err_str(what: &str, msg: impl std::fmt::Display) -> NapiError {
    NapiError::from_reason(format!("{what}: {msg}"))
}

/// Walk `error`'s tree, collecting concrete error references in traversal order.
///
/// A nested [`GixError`] boundary owns a full tree, visited in logical order by
/// [`GixError::iter_errors()`] (including native sources); plain errors are followed through
/// their standard `source()` chain.
fn collect_chain<'a>(
    error: &'a (dyn std::error::Error + 'static),
    out: &mut Vec<&'a (dyn std::error::Error + 'static)>,
    depth: usize,
) {
    if depth > MAX_CAUSES {
        return;
    }
    if let Some(tree) = error.downcast_ref::<GixError>() {
        out.extend(tree.iter_errors());
        return;
    }
    out.push(error);
    if let Some(source) = error.source() {
        collect_chain(source, out, depth + 1);
    }
}

/// Convert the error tree into a nested chain of [`NapiError`]s for JS `error.cause`.
fn build_cause(error: &(dyn std::error::Error + 'static)) -> Option<Box<NapiError>> {
    let mut refs = Vec::new();
    collect_chain(error, &mut refs, 0);

    // Every entry gets a cause, including the root (whose diagnostic is also
    // in the message): single-node trees must still expose their classification.
    let mut messages: Vec<String> = Vec::with_capacity(refs.len());
    for entry in refs.iter().copied() {
        let mut message = entry.to_string();
        if message.is_empty() {
            continue;
        }
        if let Some(classification) = gix::error::classify(entry).next() {
            message.push_str(&format!(" [git: {:?}]", classification.class()));
        }
        if messages.last().is_some_and(|last| last == &message) {
            continue;
        }
        messages.push(message);
        if messages.len() >= MAX_CAUSES {
            break;
        }
    }

    let mut acc: Option<Box<NapiError>> = None;
    for message in messages.iter().rev() {
        let mut cause = Box::new(NapiError::from_reason(message.clone()));
        cause.cause = acc;
        acc = Some(cause);
    }
    acc
}

/// Parse a hex object id string into a [`gix::ObjectId`].
pub fn parse_oid(s: &str) -> napi::Result<gix::ObjectId> {
    gix::ObjectId::from_hex(s.as_bytes()).map_err(|e| err("parse_oid", e))
}

/// Obtain a cheap thread-local view of the repository for synchronous methods.
///
/// Re-applies the cached reference namespace and packed-refs mmap threshold, since
/// those live on the thread-local view and would otherwise be reset on every call.
pub fn thread_local(repo: &Arc<gix::ThreadSafeRepository>) -> gix::Repository {
    let mut r = repo.to_thread_local();
    let key = Arc::as_ptr(repo) as usize;
    if let Some(ns) = cached_namespace_get(key) {
        // The cached namespace was validated when set.
        let _ = r.set_namespace(&ns);
    }
    if let Some(bytes) = cached_mmap_threshold_get(key) {
        r.refs.set_packed_buffer_mmap_threshold(bytes);
    }
    r
}

thread_local! {
    /// Per-repository reference namespace cache. Each synchronous call gets a fresh
    /// thread-local view whose in-memory namespace would not otherwise persist across
    /// calls, so we mirror the last value here (keyed by the repo allocation).
    static NAMESPACE_CACHE: std::cell::RefCell<std::collections::HashMap<usize, Option<String>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    /// Per-repository packed-refs mmap threshold, mirrored for the same reason.
    static MMAP_THRESHOLD_CACHE: std::cell::RefCell<std::collections::HashMap<usize, u64>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Read the cached reference namespace for the repository identified by `key`.
pub fn cached_namespace_get(key: usize) -> Option<String> {
    NAMESPACE_CACHE.with(|m| m.borrow().get(&key).cloned().flatten())
}

/// Cache the reference namespace (or `None` to clear it) for the repository identified by `key`.
pub fn cached_namespace_set(key: usize, value: Option<String>) {
    NAMESPACE_CACHE.with(|m| {
        m.borrow_mut().insert(key, value);
    });
}

/// Read the cached packed-refs mmap threshold for the repository identified by `key`.
pub fn cached_mmap_threshold_get(key: usize) -> Option<u64> {
    MMAP_THRESHOLD_CACHE.with(|m| m.borrow().get(&key).copied())
}

/// Cache the packed-refs mmap threshold for the repository identified by `key`.
pub fn cached_mmap_threshold_set(key: usize, value: u64) {
    MMAP_THRESHOLD_CACHE.with(|m| {
        m.borrow_mut().insert(key, value);
    });
}
