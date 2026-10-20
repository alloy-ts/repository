use std::sync::Arc;

use napi::Error as NapiError;

/// Build a [`napi::Error`] from a domain label and an underlying error.
pub fn err(what: &str, e: impl std::fmt::Display) -> NapiError {
    NapiError::from_reason(format!("{what}: {e}"))
}

/// Parse a hex object id string into a [`gix::ObjectId`].
pub fn parse_oid(s: &str) -> napi::Result<gix::ObjectId> {
    gix::ObjectId::from_hex(s.as_bytes()).map_err(|e| err("parse_oid", e))
}

/// Obtain a cheap thread-local view of the repository for synchronous methods.
pub fn thread_local(repo: &Arc<gix::ThreadSafeRepository>) -> gix::Repository {
    repo.to_thread_local()
}

thread_local! {
    /// Per-repository reference namespace cache. Each synchronous call gets a fresh
    /// thread-local view whose in-memory namespace would not otherwise persist across
    /// calls, so we mirror the last value here (keyed by the repo allocation).
    static NAMESPACE_CACHE: std::cell::RefCell<std::collections::HashMap<usize, Option<String>>> =
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
