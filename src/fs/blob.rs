//! NAPI bindings for Blob conforming to W3C / MDN Blob API.

use napi::bindgen_prelude::*;
use napi_derive::napi;

use crate::error::err_str;

/// Options passed when constructing a [`Blob`].
#[napi(object)]
#[derive(Default, Clone)]
pub struct BlobPropertyBag {
    pub r#type: Option<String>,
    pub endings: Option<String>,
}

/// A Blob object represents a blob, which is a file-like object of immutable, raw data;
/// they can be read as text or binary data, or converted into a ReadableStream.
#[napi(js_name = "W3cBlob")]
#[derive(Clone)]
pub struct FsBlob {
    pub(crate) data: Vec<u8>,
    pub(crate) mime_type: String,
}

#[napi]
impl FsBlob {
    /// Construct a new Blob from an array of parts (strings or Buffers/Uint8Arrays) and optional options bag.
    #[napi(constructor)]
    pub fn new(
        blob_parts: Option<Vec<Either<String, Buffer>>>,
        options: Option<BlobPropertyBag>,
    ) -> Result<Self> {
        let mut data = Vec::new();
        if let Some(parts) = blob_parts {
            for part in parts {
                match part {
                    Either::A(s) => data.extend_from_slice(s.as_bytes()),
                    Either::B(b) => data.extend_from_slice(&b),
                }
            }
        }

        let mime_type = options
            .and_then(|o| o.r#type)
            .unwrap_or_else(String::new)
            .to_ascii_lowercase();

        Ok(FsBlob { data, mime_type })
    }

    /// Size of the blob data in bytes.
    #[napi(getter)]
    pub fn size(&self) -> u32 {
        self.data.len() as u32
    }

    /// The MIME type of the data contained in the Blob object.
    #[napi(getter, js_name = "type")]
    pub fn get_type(&self) -> String {
        self.mime_type.clone()
    }

    /// Returns a Promise that resolves with a string containing the entire contents of the Blob as UTF-8 text.
    #[napi(ts_return_type = "Promise<string>")]
    pub async fn text(&self) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.data).into_owned())
    }

    /// Returns a Promise that resolves with the parsed JSON contents of the Blob.
    #[napi(ts_return_type = "Promise<any>")]
    pub async fn json(&self) -> Result<serde_json::Value> {
        serde_json::from_slice(&self.data).map_err(|e| err_str("Blob.json", e.to_string()))
    }

    /// Returns a Promise that resolves with an ArrayBuffer containing the entire contents of the Blob as binary data.
    #[napi(ts_return_type = "Promise<Buffer>")]
    pub async fn array_buffer(&self) -> Result<Buffer> {
        Ok(Buffer::from(self.data.clone()))
    }

    /// Returns a Promise that resolves with a Uint8Array containing the contents of the blob.
    #[napi(ts_return_type = "Promise<Buffer>")]
    pub async fn bytes(&self) -> Result<Buffer> {
        Ok(Buffer::from(self.data.clone()))
    }

    /// Returns a ReadableStream that can be used to read the contents of the Blob.
    #[napi]
    pub fn stream(&self, env: Env) -> Result<Unknown> {
        let buf = Buffer::from(self.data.clone());
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

    /// Returns a new Blob object containing the data in the specified range of bytes of the blob on which it's called.
    #[napi]
    pub fn slice(
        &self,
        start: Option<i64>,
        end: Option<i64>,
        content_type: Option<String>,
    ) -> Result<FsBlob> {
        let len = self.data.len() as i64;
        let s = match start {
            Some(n) if n < 0 => (len + n).max(0) as usize,
            Some(n) => n.min(len) as usize,
            None => 0,
        };
        let e = match end {
            Some(n) if n < 0 => (len + n).max(0) as usize,
            Some(n) => n.min(len) as usize,
            None => len as usize,
        };

        let slice_data = if s < e && s < self.data.len() {
            self.data[s..e.min(self.data.len())].to_vec()
        } else {
            Vec::new()
        };

        let mime = content_type.unwrap_or_else(|| self.mime_type.clone());

        Ok(FsBlob {
            data: slice_data,
            mime_type: mime,
        })
    }
}
