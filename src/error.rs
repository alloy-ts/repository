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
