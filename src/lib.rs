mod content;
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
pub mod repo;
mod req;
pub mod task;

// Re-export repository as repo for backward compatibility if needed
pub use repo as repository;
pub use repo::Repository;
#[cfg(feature = "crypto")]
pub use crypto::vault;
pub use task::promise;
pub use task::scheduler;
pub use task::thread;

// WAL napi-binding scaffold. Gated until the surrounding core dependencies
// (`crate::types`, `crate::vfs`, `crate::read_limits`, `crate::sys`, …) exist.
#[cfg(feature = "wal")]
pub(crate) mod wal;

// Content store napi-binding scaffold (read + refold + debris + WAL mode). Composes the cores
// that live in the core crate elsewhere; gated until they exist. Enables `wal` so the WAL-backed
// write path can reach `crate::wal::Wal`.
#[cfg(feature = "store")]
pub(crate) mod content_store;

// `Record` napi class (the domain record, content program, attributes) and its JS<->core bridges.
#[cfg(feature = "record")]
pub(crate) mod record;

// Filesystem-seam napi bindings, wrapping `crate::vfs` without modifying it.
#[cfg(feature = "fs")]
pub(crate) mod fs;
