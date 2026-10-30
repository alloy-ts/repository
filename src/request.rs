//! NAPI bindings for HTTP requests backed by `reqwest`.

use std::collections::HashMap;
use std::time::Duration;

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;
use reqwest::header::{HeaderName, HeaderValue};

use crate::error::err_str;

/// HTTP Request options.
#[napi(object)]
pub struct HttpRequestOptions {
    /// Target URL.
    pub url: String,
    /// HTTP method ("GET", "POST", "PUT", "DELETE", etc.). Default: "GET".
    pub method: Option<String>,
    /// Request headers key-value map.
    pub headers: Option<HashMap<String, String>>,
    /// Optional request body buffer.
    pub body: Option<Buffer>,
    /// Timeout in milliseconds.
    pub timeout_ms: Option<u32>,
}

/// Response returned from an HTTP request.
#[napi(object)]
pub struct HttpResponse {
    /// HTTP status code (e.g. 200).
    pub status: u16,
    /// Response status text or description.
    pub status_text: String,
    /// Response headers key-value map.
    pub headers: HashMap<String, String>,
    /// Response body bytes.
    pub body: Buffer,
}

/// Asynchronous HTTP client using `reqwest`.
#[napi]
pub struct HttpClient {
    inner: reqwest::Client,
}

#[napi]
impl HttpClient {
    /// Create a new `HttpClient`.
    #[napi(constructor)]
    pub fn new() -> napi::Result<Self> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| err_str("http.client", e.to_string()))?;
        Ok(HttpClient { inner: client })
    }

    /// Execute an HTTP request asynchronously.
    #[napi]
    pub async fn request(&self, options: HttpRequestOptions) -> napi::Result<HttpResponse> {
        let method = match options.method.as_deref() {
            Some(m) => m
                .parse::<reqwest::Method>()
                .map_err(|e| err_str("http.method", e.to_string()))?,
            None => reqwest::Method::GET,
        };

        let mut req = self.inner.request(method, &options.url);

        if let Some(headers) = options.headers {
            for (k, v) in headers {
                if let (Ok(hn), Ok(hv)) = (
                    k.parse::<HeaderName>(),
                    v.parse::<HeaderValue>(),
                ) {
                    req = req.header(hn, hv);
                }
            }
        }

        if let Some(body) = options.body {
            req = req.body(body.to_vec());
        }

        if let Some(timeout_ms) = options.timeout_ms {
            req = req.timeout(Duration::from_millis(timeout_ms as u64));
        }

        let res = req
            .send()
            .await
            .map_err(|e| err_str("http.send", e.to_string()))?;

        let status = res.status().as_u16();
        let status_text = res
            .status()
            .canonical_reason()
            .unwrap_or("")
            .to_string();

        let mut res_headers = HashMap::new();
        for (k, v) in res.headers() {
            if let Ok(val) = v.to_str() {
                res_headers.insert(k.as_str().to_string(), val.to_string());
            }
        }

        let bytes = res
            .bytes()
            .await
            .map_err(|e| err_str("http.body", e.to_string()))?;

        Ok(HttpResponse {
            status,
            status_text,
            headers: res_headers,
            body: Buffer::from(bytes.to_vec()),
        })
    }
}

/// Convenience function for performing a single HTTP request using reqwest.
#[napi]
pub async fn fetch(options: HttpRequestOptions) -> napi::Result<HttpResponse> {
    let client = HttpClient::new()?;
    client.request(options).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_http_client_creation() {
        let client = HttpClient::new();
        assert!(client.is_ok());
    }
}
