use std::sync::Arc;

use napi::bindgen_prelude::Error as NapiError;
use napi::Result;

/// Map any displayable gix (or other) error into an `napi::Error` with a context prefix.
pub(crate) fn err<E: std::fmt::Display>(ctx: &str, e: E) -> NapiError {
    NapiError::from_reason(format!("{ctx}: {e}"))
}

/// Convenience macro: `$expr.map_err(|e| err($ctx, e))`.
#[macro_export]
macro_rules! map_err {
    ($expr:expr, $ctx:expr) => {
        $expr.map_err(|e| $crate::error::err($ctx, e))
    };
}

/// Parse a hex object id (40 chars for Sha1, 64 for Sha256) into a `gix::ObjectId`.
pub(crate) fn parse_oid(s: &str) -> Result<gix::ObjectId> {
    gix::ObjectId::from_hex(s.as_bytes())
        .map_err(|e| NapiError::from_reason(format!("invalid object id '{s}': {e}")))
}

/// Cheaply obtain a thread-local `gix::Repository` view from the shared, thread-safe handle.
pub(crate) fn thread_local(repo: &Arc<gix::ThreadSafeRepository>) -> gix::Repository {
    repo.to_thread_local()
}
