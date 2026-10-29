mod apply;
mod attributes;
mod auth;
mod blame;
mod blob;
mod branch;
mod checkout;
mod cherry_pick;
mod clone;
mod commit;
mod commit_graph;
mod config;
mod conflict;
mod content;
#[cfg(feature = "crypto")]
mod crypto;
mod describe;
mod diff;
mod dirwalk;
mod entry;
mod error;
mod export;
mod fetch;
mod filter;
mod format;
pub mod gpg;
mod head;
mod identity;
mod import;
mod index;
mod init;
mod log;
mod mailmap;
mod merge;
mod metrics;
mod namespace;
mod negotiate;
mod note;
mod object;
mod object_store;
mod output;
mod pack;
mod patch;
mod path;
mod pathspec;
#[cfg(feature = "gpg")]
mod pgp;
mod process;
mod progress;
mod promise;
pub(crate) mod protocol;
mod pull;
mod push;
mod rebase;
mod ref_store;
mod reference;
mod reflog;
mod refspec;
pub(crate) mod remote;
mod replace;
mod repository;
mod req;
mod reset;
mod revert;
mod revision;
mod scheduler;
mod stash;
mod status;
mod submodule;
mod tag;
mod task;
mod testament;
mod thread;
mod tree;
mod vault;
mod worktree;

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
