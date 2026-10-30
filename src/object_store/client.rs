//! NAPI bindings for ObjectStore client operations.

use napi::bindgen_prelude::*;
use napi_derive::napi;

/// Options for configuring an `ObjectStoreClient`.
#[napi(object)]
pub struct ObjectStoreClientOptions {
    /// Request timeout in milliseconds.
    pub timeout_ms: Option<u32>,
    /// User agent string.
    pub user_agent: Option<String>,
    /// Proxy URL.
    pub proxy_url: Option<String>,
    /// Whether to allow HTTP (insecure) connections.
    pub allow_http: Option<bool>,
}

/// NAPI binding client for object store HTTP/SSH communication.
#[napi]
pub struct ObjectStoreClient {
    url: String,
    options: ObjectStoreClientOptions,
}

#[napi]
impl ObjectStoreClient {
    #[napi(constructor)]
    pub fn new(url: String, options: Option<ObjectStoreClientOptions>) -> Self {
        Self {
            url,
            options: options.unwrap_or(ObjectStoreClientOptions {
                timeout_ms: None,
                user_agent: None,
                proxy_url: None,
                allow_http: None,
            }),
        }
    }

    /// Target URL of the client.
    #[napi(getter)]
    pub fn url(&self) -> String {
        self.url.clone()
    }

    /// Returns `true` if the client targets an SSH URL.
    #[napi(getter)]
    pub fn is_ssh(&self) -> bool {
        self.url.starts_with("ssh://") || self.url.contains('@') && self.url.contains(':')
    }

    /// Fetch raw bytes from the given path over HTTP.
    #[napi]
    pub async fn get_bytes(&self, key: String) -> napi::Result<Buffer> {
        let full_url = if self.url.ends_with('/') {
            format!("{}{}", self.url, key)
        } else {
            format!("{}/{}", self.url, key)
        };

        let mut client_builder = reqwest::Client::builder();
        if let Some(timeout) = self.options.timeout_ms {
            client_builder = client_builder.timeout(std::time::Duration::from_millis(timeout as u64));
        }
        if let Some(ref ua) = self.options.user_agent {
            client_builder = client_builder.user_agent(ua);
        }
        if let Some(ref proxy) = self.options.proxy_url {
            let p = reqwest::Proxy::all(proxy)
                .map_err(|e| napi::Error::from_reason(format!("Invalid proxy: {e}")))?;
            client_builder = client_builder.proxy(p);
        }

        let client = client_builder
            .build()
            .map_err(|e| napi::Error::from_reason(format!("Failed to build HTTP client: {e}")))?;

        let resp = client
            .get(&full_url)
            .send()
            .await
            .map_err(|e| napi::Error::from_reason(format!("GET request failed: {e}")))?;

        if !resp.status().is_success() {
            return Err(napi::Error::from_reason(format!(
                "HTTP request returned status {}",
                resp.status()
            )));
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| napi::Error::from_reason(format!("Failed to read response body: {e}")))?;

        Ok(Buffer::from(bytes.to_vec()))
    }

    /// Put raw bytes to the given key over HTTP.
    #[napi]
    pub async fn put_bytes(&self, key: String, body: Buffer) -> napi::Result<()> {
        let full_url = if self.url.ends_with('/') {
            format!("{}{}", self.url, key)
        } else {
            format!("{}/{}", self.url, key)
        };

        let mut client_builder = reqwest::Client::builder();
        if let Some(timeout) = self.options.timeout_ms {
            client_builder = client_builder.timeout(std::time::Duration::from_millis(timeout as u64));
        }

        let client = client_builder
            .build()
            .map_err(|e| napi::Error::from_reason(format!("Failed to build HTTP client: {e}")))?;

        let resp = client
            .put(&full_url)
            .body(body.to_vec())
            .send()
            .await
            .map_err(|e| napi::Error::from_reason(format!("PUT request failed: {e}")))?;

        if !resp.status().is_success() {
            return Err(napi::Error::from_reason(format!(
                "HTTP PUT returned status {}",
                resp.status()
            )));
        }

        Ok(())
    }
}
