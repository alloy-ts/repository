//! NAPI binding scaffold for the filesystem seam (`crate::vfs`, defined in
//! `src/object_store/loose.rs`).
//!
//! These wrappers expose the WRITE-SIDE fs operations to JavaScript. The seam itself is NOT
//! modified — these functions call into `crate::vfs::*`, which remains the single, complete,
//! DST-instrumented crash-safety surface. (If the module is named differently in this crate,
//! adjust the `crate::vfs` paths below.)
//!
//! This is a BINDING SCAFFOLD and will not compile until `crate::vfs` / `crate::sys` exist.

#![allow(dead_code, unused_variables, unreachable_code, unused_imports)]

use std::path::Path;

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

fn to_napi(e: std::io::Error) -> napi::Error {
    napi::Error::from_reason(e.to_string())
}

/// Truncate-or-create the file at `path`.
#[napi]
pub fn create(path: String) -> napi::Result<()> {
    crate::vfs::create(Path::new(&path)).map_err(to_napi)
}

/// Write `data` to `path` wholesale.
#[napi]
pub fn write_file(path: String, data: Buffer) -> napi::Result<()> {
    crate::vfs::write_file(Path::new(&path), &data).map_err(to_napi)
}

/// Read the whole file at `path`.
#[napi]
pub fn read_file(path: String) -> napi::Result<Buffer> {
    let bytes = crate::vfs::read_file(Path::new(&path)).map_err(to_napi)?;
    Ok(bytes.into())
}

/// Whether `path` exists (including a name this process created but has not yet published).
#[napi]
pub fn exists(path: String) -> bool {
    crate::vfs::exists(Path::new(&path))
}

/// Open `path` read-write and fsync it.
#[napi]
pub fn sync_file(path: String) -> napi::Result<()> {
    let f = crate::vfs::open_rw(Path::new(&path)).map_err(to_napi)?;
    crate::vfs::sync_file(&f, Path::new(&path)).map_err(to_napi)
}

/// Fsync the directory containing `path` (publishes pending Windows names).
#[napi]
pub fn sync_dir(path: String) -> napi::Result<()> {
    crate::vfs::sync_dir(Path::new(&path)).map_err(to_napi)
}

/// Rename `from` to `to`.
#[napi]
pub fn rename(from: String, to: String) -> napi::Result<()> {
    crate::vfs::rename(Path::new(&from), Path::new(&to)).map_err(to_napi)
}

/// Install `from` at `to` without replacement.
#[napi]
pub fn rename_noreplace(from: String, to: String) -> napi::Result<()> {
    crate::vfs::rename_noreplace(Path::new(&from), Path::new(&to)).map_err(to_napi)
}

/// Remove the file at `path`.
#[napi]
pub fn unlink(path: String) -> napi::Result<()> {
    crate::vfs::unlink(Path::new(&path)).map_err(to_napi)
}

/// Create `path` and all ancestors.
#[napi]
pub fn mkdir_all(path: String) -> napi::Result<()> {
    crate::vfs::mkdir_all(Path::new(&path)).map_err(to_napi)
}

/// Recursively remove `path`.
#[napi]
pub fn remove_tree(path: String) -> napi::Result<()> {
    crate::vfs::remove_tree(Path::new(&path)).map_err(to_napi)
}
