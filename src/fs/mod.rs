//! Filesystem module providing Node.js `node:fs` compatible APIs along with W3C Blob and File classes.

pub mod blob;
pub mod file;

pub use self::blob::FsBlob;
pub use self::file::File;

use std::fs;
use std::path::Path;

use napi::bindgen_prelude::*;
use napi_derive::napi;

use crate::error::err_str;

/// Synchronous file read returning Buffer.
#[napi(js_name = "fsReadFileSync")]
pub fn read_file_sync(path: String) -> Result<Buffer> {
    let bytes = fs::read(&path).map_err(|e| err_str("fsReadFileSync", e.to_string()))?;
    Ok(Buffer::from(bytes))
}

/// Asynchronous file read returning Buffer.
#[napi(js_name = "fsReadFile", ts_return_type = "Promise<Buffer>")]
pub async fn read_file(path: String) -> Result<Buffer> {
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|e| err_str("fsReadFile", e.to_string()))?;
    Ok(Buffer::from(bytes))
}

/// Synchronous file write.
#[napi(js_name = "fsWriteFileSync")]
pub fn write_file_sync(path: String, data: Either<String, Buffer>) -> Result<()> {
    let bytes = match data {
        Either::A(s) => s.into_bytes(),
        Either::B(b) => b.to_vec(),
    };
    fs::write(&path, bytes).map_err(|e| err_str("fsWriteFileSync", e.to_string()))?;
    Ok(())
}

/// Asynchronous file write.
#[napi(js_name = "fsWriteFile", ts_return_type = "Promise<void>")]
pub async fn write_file(path: String, data: Either<String, Buffer>) -> Result<()> {
    let bytes = match data {
        Either::A(s) => s.into_bytes(),
        Either::B(b) => b.to_vec(),
    };
    tokio::fs::write(&path, bytes)
        .await
        .map_err(|e| err_str("fsWriteFile", e.to_string()))?;
    Ok(())
}

/// Synchronous check if path exists.
#[napi(js_name = "fsExistsSync")]
pub fn exists_sync(path: String) -> bool {
    Path::new(&path).exists()
}

/// Synchronous unlink (remove file).
#[napi(js_name = "fsUnlinkSync")]
pub fn unlink_sync(path: String) -> Result<()> {
    fs::remove_file(&path).map_err(|e| err_str("fsUnlinkSync", e.to_string()))
}

/// Synchronous directory removal.
#[napi(js_name = "fsRmdirSync")]
pub fn rmdir_sync(path: String) -> Result<()> {
    fs::remove_dir(&path).map_err(|e| err_str("fsRmdirSync", e.to_string()))
}

/// Synchronous remove with recursive option.
#[napi(js_name = "fsRmSync")]
pub fn rm_sync(path: String, options: Option<file::DeleteOptions>) -> Result<()> {
    let p = Path::new(&path);
    if !p.exists() {
        return Ok(());
    }
    let recursive = options.and_then(|o| o.recursive).unwrap_or(false);
    if p.is_dir() {
        if recursive {
            fs::remove_dir_all(p).map_err(|e| err_str("fsRmSync", e.to_string()))?;
        } else {
            fs::remove_dir(p).map_err(|e| err_str("fsRmSync", e.to_string()))?;
        }
    } else {
        fs::remove_file(p).map_err(|e| err_str("fsRmSync", e.to_string()))?;
    }
    Ok(())
}

/// Synchronous mkdir.
#[napi(js_name = "fsMkdirSync")]
pub fn mkdir_sync(path: String, recursive: Option<bool>) -> Result<()> {
    if recursive.unwrap_or(false) {
        fs::create_dir_all(&path).map_err(|e| err_str("fsMkdirSync", e.to_string()))
    } else {
        fs::create_dir(&path).map_err(|e| err_str("fsMkdirSync", e.to_string()))
    }
}

/// Synchronous readdir.
#[napi(js_name = "fsReaddirSync")]
pub fn readdir_sync(path: String) -> Result<Vec<String>> {
    let entries = fs::read_dir(&path).map_err(|e| err_str("fsReaddirSync", e.to_string()))?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| err_str("fsReaddirSync", e.to_string()))?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    Ok(names)
}
