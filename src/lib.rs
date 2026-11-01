#[cfg(feature = "crypto")]
pub mod crypto;
mod error;
mod format;
pub mod gpg;
mod metrics;
mod object_store;
mod output;
#[cfg(feature = "gpg")]
mod pgp;
mod process;
mod promise;
pub mod repo;
mod req;
mod scheduler;
mod task;
mod thread;

// WAL napi-binding scaffold.
#[cfg(feature = "wal")]
pub(crate) mod wal;

// Content store napi-binding scaffold.
#[cfg(feature = "store")]
pub(crate) mod content_store;

// `Record` napi class.
#[cfg(feature = "record")]
pub(crate) mod record;

// Filesystem-seam napi bindings.
#[cfg(feature = "fs")]
pub(crate) mod fs;

#[cfg(not(feature = "crypto"))]
pub mod crypto;
