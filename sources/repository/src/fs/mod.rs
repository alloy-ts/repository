//! File system module providing `node:fs` compatible APIs and the `File` / `Blob` classes.

pub mod blob;
pub mod file;

pub use blob::*;
pub use file::*;

use std::fs;
use std::path::Path;
use napi::bindgen_prelude::*;
use napi_derive::napi;

fn to_napi(e: std::io::Error) -> napi::Error {
    napi::Error::from_reason(e.to_string())
}

/// Asynchronously read file.
#[napi]
pub async fn fs_read_file(path: String) -> napi::Result<Buffer> {
    let bytes = tokio::fs::read(&path).await.map_err(to_napi)?;
    Ok(bytes.into())
}

/// Read the whole file at `path` synchronously.
#[napi]
pub fn fs_read_file_sync(path: String) -> napi::Result<Buffer> {
    let bytes = fs::read(&path).map_err(to_napi)?;
    Ok(bytes.into())
}

/// Asynchronously write file.
#[napi]
pub async fn fs_write_file(path: String, data: Either<String, Buffer>) -> napi::Result<()> {
    let bytes = match data {
        Either::A(s) => s.into_bytes(),
        Either::B(b) => b.to_vec(),
    };
    tokio::fs::write(&path, bytes).await.map_err(to_napi)
}

/// Write data to file synchronously.
#[napi]
pub fn fs_write_file_sync(path: String, data: Either<String, Buffer>) -> napi::Result<()> {
    let bytes = match data {
        Either::A(s) => s.into_bytes(),
        Either::B(b) => b.to_vec(),
    };
    fs::write(&path, bytes).map_err(to_napi)
}

/// Check if a path exists synchronously.
#[napi]
pub fn fs_exists_sync(path: String) -> bool {
    Path::new(&path).exists()
}

/// Unlink (delete) a file.
#[napi]
pub fn fs_unlink_sync(path: String) -> napi::Result<()> {
    fs::remove_file(&path).map_err(to_napi)
}

/// Create a directory.
#[napi]
pub fn fs_mkdir_sync(path: String, recursive: Option<bool>) -> napi::Result<()> {
    if recursive.unwrap_or(false) {
        fs::create_dir_all(&path).map_err(to_napi)
    } else {
        fs::create_dir(&path).map_err(to_napi)
    }
}

/// Remove a directory.
#[napi]
pub fn fs_rmdir_sync(path: String) -> napi::Result<()> {
    fs::remove_dir(&path).map_err(to_napi)
}
