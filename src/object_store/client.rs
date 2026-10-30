//! NAPI bindings for remote Object Store client (`ObjectStoreClient`).

use std::collections::HashMap;
use std::time::Duration;

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use crate::error::err;

/// Options for configuring an [`ObjectStoreClient`].
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct ObjectStoreClientOptions {
    /// Custom HTTP headers to include with requests.
    pub headers: Option<HashMap<String, String>>,
    /// Request timeout in milliseconds. Defaults to 30000ms.
    pub timeout_ms: Option<u32>,
}

/// Object metadata returned by [`ObjectStoreClient`].
#[napi(object)]
#[derive(Debug, Clone)]
pub struct ObjectStoreMeta {
    pub location: String,
    pub size: f64,
    pub last_modified: Option<f64>,
    pub etag: Option<String>,
}

/// Client for communicating with remote HTTP object stores.
#[napi]
pub struct ObjectStoreClient {
    client: reqwest::Client,
    base_url: String,
}

#[napi]
impl ObjectStoreClient {
    /// Create a new `ObjectStoreClient` pointing at `baseUrl`.
    #[napi(constructor)]
    pub fn new(base_url: String, options: Option<ObjectStoreClientOptions>) -> napi::Result<Self> {
        let opts = options.unwrap_or_default();
        let timeout = Duration::from_millis(opts.timeout_ms.unwrap_or(30000) as u64);

        let mut builder = reqwest::Client::builder()
            .timeout(timeout)
            .user_agent("repository/object-store");

        if let Some(headers) = opts.headers {
            let mut header_map = reqwest::header::HeaderMap::new();
            for (k, v) in headers {
                if let (Ok(name), Ok(val)) = (
                    reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                    reqwest::header::HeaderValue::from_str(&v),
                ) {
                    header_map.insert(name, val);
                }
            }
            builder = builder.default_headers(header_map);
        }

        let client = builder.build().map_err(|e| err("object_store_client.build", e))?;
        let base_url = base_url.trim_end_matches('/').to_string();

        Ok(Self { client, base_url })
    }

    fn url_for(&self, location: &str) -> String {
        let loc = location.trim_start_matches('/');
        if loc.is_empty() {
            self.base_url.clone()
        } else {
            format!("{}/{}", self.base_url, loc)
        }
    }

    /// Retrieve object data as a Buffer.
    #[napi]
    pub async fn get(&self, location: String) -> napi::Result<Buffer> {
        let url = self.url_for(&location);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| err("object_store_client.get", e))?;

        if !resp.status().is_success() {
            return Err(napi::Error::from_reason(format!(
                "HTTP request failed with status: {}",
                resp.status()
            )));
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| err("object_store_client.bytes", e))?;
        Ok(Buffer::from(bytes.to_vec()))
    }

    /// Store payload bytes at the specified location.
    #[napi]
    pub async fn put(&self, location: String, payload: Buffer) -> napi::Result<()> {
        let url = self.url_for(&location);
        let resp = self
            .client
            .put(&url)
            .body(payload.to_vec())
            .send()
            .await
            .map_err(|e| err("object_store_client.put", e))?;

        if !resp.status().is_success() {
            return Err(napi::Error::from_reason(format!(
                "HTTP PUT failed with status: {}",
                resp.status()
            )));
        }

        Ok(())
    }

    /// Delete object at the specified location.
    #[napi]
    pub async fn delete(&self, location: String) -> napi::Result<()> {
        let url = self.url_for(&location);
        let resp = self
            .client
            .delete(&url)
            .send()
            .await
            .map_err(|e| err("object_store_client.delete", e))?;

        if !resp.status().is_success() {
            return Err(napi::Error::from_reason(format!(
                "HTTP DELETE failed with status: {}",
                resp.status()
            )));
        }

        Ok(())
    }

    /// Fetch metadata for an object without downloading its body.
    #[napi]
    pub async fn head(&self, location: String) -> napi::Result<ObjectStoreMeta> {
        let url = self.url_for(&location);
        let resp = self
            .client
            .head(&url)
            .send()
            .await
            .map_err(|e| err("object_store_client.head", e))?;

        if !resp.status().is_success() {
            return Err(napi::Error::from_reason(format!(
                "HTTP HEAD failed with status: {}",
                resp.status()
            )));
        }

        let size = resp
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);

        let etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        Ok(ObjectStoreMeta {
            location,
            size,
            last_modified: None,
            etag,
        })
    }
}
