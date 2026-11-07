//! NAPI bindings for the `File` class.
//!
//! Constructor `new(urlOrPath, options?)` supports string path, file URL string, or URL object.
//! Supports pointing to a location on disk where a file does not exist.
//! Supports `.exists()`, `.write()`, `.delete({ recursive?: boolean })`, `.text()`, `.json()`,
//! `.bytes()`, `.arrayBuffer()`, `.stream()`.
//! Default MIME type is `"text/plain;charset=utf-8"`, overridable by passing RequestOpts from src/net/request.rs.

use std::fs;
use std::path::{Path, PathBuf};
use napi::bindgen_prelude::*;
use napi_derive::napi;
use napi::Status;

use crate::net::request::RequestOpts;

/// Options for file deletion.
#[napi(object)]
#[derive(Default)]
pub struct DeleteOptions {
    pub recursive: Option<bool>,
}

/// Web & Bun compatible File object pointing to disk paths.
#[napi]
pub struct File {
    path: PathBuf,
    mime_type: String,
}

#[napi]
impl File {
    #[napi(constructor)]
    pub fn new(
        url_or_path: Either<String, Object>,
        options: Option<RequestOpts>,
    ) -> napi::Result<Self> {
        let path_str = match url_or_path {
            Either::A(s) => {
                if s.starts_with("file://") {
                    s.trim_start_matches("file://").to_string()
                } else {
                    s
                }
            }
            Either::B(obj) => {
                // Handle new URL(...) object (e.g. href, pathname)
                if let Ok(pathname) = obj.get_named_property::<String>("pathname") {
                    pathname
                } else if let Ok(href) = obj.get_named_property::<String>("href") {
                    href.trim_start_matches("file://").to_string()
                } else {
                    return Err(napi::Error::new(
                        Status::InvalidArg,
                        "expected string path or URL object",
                    ));
                }
            }
        };

        let mut mime_type = "text/plain;charset=utf-8".to_string();
        if let Some(opts) = options {
            if let Some(headers) = opts.headers {
                if let Some(ct) = headers.get("content-type") {
                    mime_type = ct.clone();
                } else if let Some(ct) = headers.get("Content-Type") {
                    mime_type = ct.clone();
                }
            }
        }

        Ok(File {
            path: PathBuf::from(path_str),
            mime_type,
        })
    }

    #[napi(getter)]
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    }

    #[napi(getter)]
    pub fn size(&self) -> f64 {
        if let Ok(metadata) = fs::metadata(&self.path) {
            metadata.len() as f64
        } else {
            0.0
        }
    }

    #[napi(getter, js_name = "type")]
    pub fn mime_type(&self) -> String {
        self.mime_type.clone()
    }

    /// Check if the file exists on disk.
    #[napi]
    pub async fn exists(&self) -> napi::Result<bool> {
        Ok(self.path.exists())
    }

    /// Read file content as UTF-8 string.
    #[napi]
    pub async fn text(&self) -> napi::Result<String> {
        tokio::fs::read_to_string(&self.path).await.map_err(|e| {
            napi::Error::new(Status::GenericFailure, format!("failed to read file: {e}"))
        })
    }

    /// Read and parse file content as JSON.
    #[napi(js_name = "json")]
    pub async fn parse_json(&self) -> napi::Result<serde_json::Value> {
        let content = self.text().await?;
        serde_json::from_str(&content).map_err(|e| {
            napi::Error::new(Status::GenericFailure, format!("failed to parse json: {e}"))
        })
    }

    /// Read file content as Buffer / ArrayBuffer.
    #[napi]
    pub async fn bytes(&self) -> napi::Result<Buffer> {
        let b = tokio::fs::read(&self.path).await.map_err(|e| {
            napi::Error::new(Status::GenericFailure, format!("failed to read file: {e}"))
        })?;
        Ok(b.into())
    }

    #[napi(js_name = "arrayBuffer")]
    pub async fn array_buffer(&self) -> napi::Result<Buffer> {
        self.bytes().await
    }

    /// Stream file content.
    #[napi]
    pub async fn stream(&self) -> napi::Result<Buffer> {
        self.bytes().await
    }

    /// Write content to the file.
    #[napi]
    pub async fn write(&self, data: Either<String, Buffer>) -> napi::Result<u32> {
        let bytes = match data {
            Either::A(s) => s.into_bytes(),
            Either::B(b) => b.to_vec(),
        };
        if let Some(parent) = self.path.parent() {
            if !parent.exists() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
        }
        tokio::fs::write(&self.path, &bytes).await.map_err(|e| {
            napi::Error::new(Status::GenericFailure, format!("failed to write file: {e}"))
        })?;
        Ok(bytes.len() as u32)
    }

    /// Delete file or directory.
    #[napi]
    pub fn delete(&self, options: Option<DeleteOptions>) -> napi::Result<()> {
        let recursive = options.and_then(|o| o.recursive).unwrap_or(false);
        if self.path.is_dir() {
            if recursive {
                fs::remove_dir_all(&self.path).map_err(|e| {
                    napi::Error::new(Status::GenericFailure, format!("delete failed: {e}"))
                })?;
            } else {
                fs::remove_dir(&self.path).map_err(|e| {
                    napi::Error::new(Status::GenericFailure, format!("delete failed: {e}"))
                })?;
            }
        } else if self.path.exists() {
            fs::remove_file(&self.path).map_err(|e| {
                napi::Error::new(Status::GenericFailure, format!("delete failed: {e}"))
            })?;
        }
        Ok(())
    }
}
