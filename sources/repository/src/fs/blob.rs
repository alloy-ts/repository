//! Web Blob API NAPI bindings.
//!
//! Conforms to https://developer.mozilla.org/en-US/docs/Web/API/Blob

use napi::bindgen_prelude::*;
use napi_derive::napi;
use napi::Status;

/// Options for constructing a [`Blob`].
#[napi(object)]
#[derive(Default, Clone)]
pub struct BlobPropertyBag {
    pub r#type: Option<String>,
    pub endings: Option<String>,
}

/// A Web-compatible `Blob` object.
#[napi(js_name = "FileBlob")]
pub struct WebBlob {
    data: Vec<u8>,
    mime_type: String,
}

#[napi]
impl WebBlob {
    #[napi(constructor)]
    pub fn new(
        blob_parts: Option<Vec<Either<String, Buffer>>>,
        options: Option<BlobPropertyBag>,
    ) -> Self {
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
            .unwrap_or_else(|| "".to_string());

        WebBlob { data, mime_type }
    }

    #[napi(getter)]
    pub fn size(&self) -> f64 {
        self.data.len() as f64
    }

    #[napi(getter, js_name = "type")]
    pub fn mime_type(&self) -> String {
        self.mime_type.clone()
    }

    /// Returns a new `Blob` object containing the data in the specified range of bytes.
    #[napi]
    pub fn slice(
        &self,
        start: Option<i64>,
        end: Option<i64>,
        content_type: Option<String>,
    ) -> Self {
        let len = self.data.len() as i64;
        let s = match start {
            Some(v) if v < 0 => (len + v).max(0) as usize,
            Some(v) => (v.min(len)).max(0) as usize,
            None => 0,
        };
        let e = match end {
            Some(v) if v < 0 => (len + v).max(0) as usize,
            Some(v) => (v.min(len)).max(0) as usize,
            None => len as usize,
        };

        let sliced = if s < e && s < self.data.len() {
            self.data[s..e.min(self.data.len())].to_vec()
        } else {
            Vec::new()
        };

        WebBlob {
            data: sliced,
            mime_type: content_type.unwrap_or_else(|| self.mime_type.clone()),
        }
    }

    /// Returns a promise that resolves with a string containing the contents of the blob.
    #[napi]
    pub async fn text(&self) -> napi::Result<String> {
        String::from_utf8(self.data.clone()).map_err(|e| {
            napi::Error::new(Status::GenericFailure, format!("invalid utf-8: {e}"))
        })
    }

    /// Read and parse blob content as JSON.
    #[napi(js_name = "json")]
    pub async fn parse_json(&self) -> napi::Result<serde_json::Value> {
        let content = self.text().await?;
        serde_json::from_str(&content).map_err(|e| {
            napi::Error::new(Status::GenericFailure, format!("failed to parse json: {e}"))
        })
    }

    /// Returns a promise that resolves with an ArrayBuffer/Buffer containing the contents.
    #[napi(js_name = "arrayBuffer")]
    pub async fn array_buffer(&self) -> napi::Result<Buffer> {
        Ok(self.data.clone().into())
    }

    /// Read raw bytes of the blob.
    #[napi]
    pub async fn bytes(&self) -> napi::Result<Buffer> {
        Ok(self.data.clone().into())
    }

    /// Returns a ReadableStream/Buffer representation of the blob.
    #[napi]
    pub async fn stream(&self) -> napi::Result<Buffer> {
        Ok(self.data.clone().into())
    }
}
