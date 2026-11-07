//! NAPI bindings for File conforming to MDN File API (based on Blob),
//! with extended disk-backed file methods (exists, write, delete, stream, etc.).

use std::fs;
use std::path::{Path, PathBuf};

use napi::bindgen_prelude::*;
use napi_derive::napi;

use crate::error::err_str;
use crate::fs::blob::FsBlob;

/// Options passed when calling `file.delete({ recursive?: boolean })`.
#[napi(object)]
#[derive(Default, Clone)]
pub struct DeleteOptions {
    pub recursive: Option<bool>,
}

/// Options passed when constructing a [`File`] with optional metadata.
#[napi(object)]
#[derive(Default, Clone)]
pub struct FilePropertyBag {
    pub r#type: Option<String>,
    pub last_modified: Option<i64>,
}

/// MDN File class representing a file on disk or in memory.
/// Can be constructed with:
/// `new File(urlOrPath, options?)`
/// or
/// `new File(blobParts, fileName, options?)`
#[napi]
#[derive(Clone)]
pub struct File {
    path: Option<PathBuf>,
    name: String,
    last_modified: i64,
    blob: FsBlob,
}

impl File {
    fn read_disk_bytes(&self) -> Vec<u8> {
        if let Some(path) = &self.path {
            if path.exists() {
                if let Ok(data) = fs::read(path) {
                    return data;
                }
            }
        }
        self.blob.data.clone()
    }

    fn read_disk_size(&self) -> u32 {
        if let Some(path) = &self.path {
            if path.exists() {
                if let Ok(meta) = fs::metadata(path) {
                    return meta.len() as u32;
                }
            }
        }
        self.blob.data.len() as u32
    }

    fn read_disk_mtime(&self) -> i64 {
        if let Some(path) = &self.path {
            if let Ok(meta) = fs::metadata(path) {
                if let Ok(mtime) = meta.modified() {
                    if let Ok(dur) = mtime.duration_since(std::time::UNIX_EPOCH) {
                        return dur.as_millis() as i64;
                    }
                }
            }
        }
        self.last_modified
    }
}

#[napi]
impl File {
    /// Construct a new `File` object.
    /// Supports:
    /// - `new File(urlOrPath, options?)`
    /// - `new File(urlObject, options?)`
    /// - `new File(blobParts, fileName, options?)`
    #[napi(constructor)]
    pub fn new(
        env: Env,
        input: Unknown,
        second_arg: Option<Unknown>,
        third_arg: Option<FilePropertyBag>,
    ) -> Result<Self> {
        let default_mime = "text/plain;charset=utf-8".to_string();
        let now_millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        let val_type = input.get_type()?;

        // Case 1: First argument is an Array (BlobParts: new File(bits, name, options))
        if input.is_array()? {
            let arr = input.coerce_to_object()?;
            let len: u32 = arr.get_array_length()?;
            let mut parts = Vec::new();
            for i in 0..len {
                let item: Unknown = arr.get_element(i)?;
                if item.is_buffer()? {
                    let buf: Buffer = unsafe { Buffer::from_napi_value(env.raw(), item.raw())? };
                    parts.push(Either::B(buf));
                } else if item.get_type()? == ValueType::String {
                    let s: String = unsafe { String::from_napi_value(env.raw(), item.raw())? };
                    parts.push(Either::A(s));
                }
            }

            let name = match second_arg {
                Some(arg) => {
                    let s: String = unsafe { String::from_napi_value(env.raw(), arg.raw())? };
                    s
                }
                None => "file".to_string(),
            };

            let mime = third_arg
                .as_ref()
                .and_then(|o| o.r#type.clone())
                .unwrap_or(default_mime);
            let last_modified = third_arg
                .as_ref()
                .and_then(|o| o.last_modified)
                .unwrap_or(now_millis);

            let blob = FsBlob::new(
                Some(parts),
                Some(crate::fs::blob::BlobPropertyBag {
                    r#type: Some(mime),
                    endings: None,
                }),
            )?;

            return Ok(File {
                path: None,
                name,
                last_modified,
                blob,
            });
        }

        // Case 2: String path or URL string or URL object
        let path_str: String = if val_type == ValueType::String {
            unsafe { String::from_napi_value(env.raw(), input.raw())? }
        } else if val_type == ValueType::Object {
            // Check for URL object (e.g. `new URL(...)`) with `.pathname` or `.href`
            let obj = input.coerce_to_object()?;
            if let Ok(Some(pathname)) = obj.get::<String>("pathname") {
                #[cfg(windows)]
                let pathname = pathname.trim_start_matches('/');
                pathname
            } else if let Ok(Some(href)) = obj.get::<String>("href") {
                if let Some(stripped) = href.strip_prefix("file://") {
                    #[cfg(windows)]
                    let stripped = stripped.trim_start_matches('/');
                    stripped.to_string()
                } else {
                    href
                }
            } else if let Ok(Some(path_val)) = obj.get::<String>("path") {
                path_val
            } else {
                return Err(err_str(
                    "File.constructor",
                    "Expected path string or URL object",
                ));
            }
        } else {
            return Err(err_str(
                "File.constructor",
                "Invalid arguments passed to File constructor",
            ));
        };

        // If path starts with file://, strip it
        let clean_path = if let Some(stripped) = path_str.strip_prefix("file://") {
            #[cfg(windows)]
            let stripped = stripped.trim_start_matches('/');
            stripped.to_string()
        } else {
            path_str
        };

        let path_buf = PathBuf::from(&clean_path);
        let name = path_buf
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&clean_path)
            .to_string();

        // Extract options: could be FilePropertyBag, or RequestOpts / RequestInit
        let mut custom_mime = None;
        let mut custom_mtime = None;

        if let Some(opts) = second_arg {
            if let Ok(obj) = opts.coerce_to_object() {
                if let Ok(Some(t)) = obj.get::<String>("type") {
                    custom_mime = Some(t);
                }
                if let Ok(Some(lm)) = obj.get::<i64>("lastModified") {
                    custom_mtime = Some(lm);
                }
                if custom_mime.is_none() {
                    if let Ok(Some(headers)) = obj.get::<std::collections::HashMap<String, String>>("headers") {
                        if let Some(ct) = headers.get("content-type").or_else(|| headers.get("Content-Type")) {
                            custom_mime = Some(ct.clone());
                        }
                    }
                }
            }
        }

        let mime = custom_mime.unwrap_or(default_mime);
        let last_mod = custom_mtime.unwrap_or(now_millis);

        let initial_data = if path_buf.exists() {
            fs::read(&path_buf).unwrap_or_default()
        } else {
            Vec::new()
        };

        let blob = FsBlob {
            data: initial_data,
            mime_type: mime,
        };

        Ok(File {
            path: Some(path_buf),
            name,
            last_modified: last_mod,
            blob,
        })
    }

    /// The name of the file.
    #[napi(getter)]
    pub fn name(&self) -> String {
        self.name.clone()
    }

    /// The last modified time of the file in milliseconds since the Unix epoch.
    #[napi(getter)]
    pub fn last_modified(&self) -> i64 {
        self.read_disk_mtime()
    }

    /// The size of the file in bytes.
    #[napi(getter)]
    pub fn size(&self) -> u32 {
        self.read_disk_size()
    }

    /// The MIME type of the file. Defaults to `text/plain;charset=utf-8`.
    #[napi(getter, js_name = "type")]
    pub fn get_type(&self) -> String {
        self.blob.mime_type.clone()
    }

    /// Returns a boolean indicating whether the file exists on disk.
    #[napi]
    pub fn exists(&self) -> bool {
        if let Some(path) = &self.path {
            path.exists()
        } else {
            false
        }
    }

    /// Writes data to the file on disk.
    /// Accepts a string, Buffer, or Uint8Array.
    #[napi(ts_return_type = "Promise<number>")]
    pub async fn write(&self, data: Either<String, Buffer>) -> Result<u32> {
        let path = match &self.path {
            Some(p) => p.clone(),
            None => {
                return Err(err_str(
                    "File.write",
                    "Cannot write to in-memory File with no disk path",
                ));
            }
        };

        let bytes = match data {
            Either::A(s) => s.into_bytes(),
            Either::B(b) => b.to_vec(),
        };

        let len = bytes.len() as u32;

        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent)?;
                }
            }
            fs::write(&path, &bytes)?;
            Ok(())
        })
        .await
        .map_err(|e| err_str("File.write", e.to_string()))?
        .map_err(|e| err_str("File.write", e.to_string()))?;

        Ok(len)
    }

    /// Delete the file from disk.
    /// Accepts optional `{ recursive?: boolean }`.
    #[napi]
    pub fn delete(&self, options: Option<DeleteOptions>) -> Result<()> {
        let path = match &self.path {
            Some(p) => p.clone(),
            None => return Ok(()),
        };

        if !path.exists() {
            return Ok(());
        }

        let recursive = options.and_then(|o| o.recursive).unwrap_or(false);

        if path.is_dir() {
            if recursive {
                fs::remove_dir_all(&path).map_err(|e| err_str("File.delete", e.to_string()))?;
            } else {
                fs::remove_dir(&path).map_err(|e| err_str("File.delete", e.to_string()))?;
            }
        } else {
            fs::remove_file(&path).map_err(|e| err_str("File.delete", e.to_string()))?;
        }

        Ok(())
    }

    /// Returns a Promise that resolves with a string containing the file contents as UTF-8 text.
    #[napi(ts_return_type = "Promise<string>")]
    pub async fn text(&self) -> Result<String> {
        let data = self.read_disk_bytes();
        Ok(String::from_utf8_lossy(&data).into_owned())
    }

    /// Returns a Promise that resolves with the parsed JSON contents of the file.
    #[napi(ts_return_type = "Promise<any>")]
    pub async fn json(&self) -> Result<serde_json::Value> {
        let data = self.read_disk_bytes();
        serde_json::from_slice(&data).map_err(|e| err_str("File.json", e.to_string()))
    }

    /// Returns a Promise that resolves with an ArrayBuffer containing the entire contents of the file.
    #[napi(ts_return_type = "Promise<Buffer>")]
    pub async fn array_buffer(&self) -> Result<Buffer> {
        let data = self.read_disk_bytes();
        Ok(Buffer::from(data))
    }

    /// Returns a Promise that resolves with a Uint8Array containing the file contents.
    #[napi(ts_return_type = "Promise<Buffer>")]
    pub async fn bytes(&self) -> Result<Buffer> {
        let data = self.read_disk_bytes();
        Ok(Buffer::from(data))
    }

    /// Returns a ReadableStream that can be used to read the contents of the file.
    #[napi]
    pub fn stream(&self, env: Env) -> Result<Unknown> {
        let data = self.read_disk_bytes();
        let buf = Buffer::from(data);
        let script = r#"
            (function(buffer) {
                if (typeof ReadableStream !== 'undefined') {
                    return new ReadableStream({
                        start(controller) {
                            controller.enqueue(buffer);
                            controller.close();
                        }
                    });
                }
                const { Readable } = require('node:stream');
                return Readable.from(buffer);
            })
        "#;
        let func: Function<Buffer, Unknown> = env.run_script(script)?;
        func.call(buf)
    }

    /// Returns a new Blob object containing a slice of the file.
    #[napi]
    pub fn slice(
        &self,
        start: Option<i64>,
        end: Option<i64>,
        content_type: Option<String>,
    ) -> Result<FsBlob> {
        let current_data = self.read_disk_bytes();
        let current_blob = FsBlob {
            data: current_data,
            mime_type: self.blob.mime_type.clone(),
        };
        current_blob.slice(start, end, content_type)
    }
}
