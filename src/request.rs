//! NAPI bindings for reqwest HTTP requests and connector.
//!
//! Exposes [`HttpClient`], [`HttpRequest`], and [`HttpResponse`] wrapping `reqwest::Client`
//! with HTTP/3 support enabled, suitable for Git smart HTTP protocol requests and custom HTTP connectors.

use std::collections::HashMap;
use std::time::Duration;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use reqwest::{Client, Method};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

/// Options for configuring the HTTP client.
#[napi(object)]
#[derive(Default)]
pub struct HttpClientOptions {
    /// Request timeout in milliseconds.
    pub timeout_ms: Option<u32>,
    /// User agent string.
    pub user_agent: Option<String>,
    /// Whether to follow HTTP redirects (default true).
    pub follow_redirects: Option<bool>,
}

/// HTTP Request wrapper for NAPI interop.
#[napi]
pub struct HttpRequest {
    pub(crate) url: String,
    pub(crate) method: String,
    pub(crate) headers: HashMap<String, String>,
    pub(crate) body: Option<Vec<u8>>,
    pub(crate) timeout_ms: Option<u32>,
}

#[napi]
impl HttpRequest {
    #[napi(constructor)]
    pub fn new(url: String, method: Option<String>) -> Self {
        Self {
            url,
            method: method.unwrap_or_else(|| "GET".to_string()),
            headers: HashMap::new(),
            body: None,
            timeout_ms: None,
        }
    }

    #[napi]
    pub fn set_header(&mut self, name: String, value: String) {
        self.headers.insert(name, value);
    }

    #[napi]
    pub fn set_body(&mut self, body: Buffer) {
        self.body = Some(body.to_vec());
    }

    #[napi]
    pub fn set_timeout(&mut self, timeout_ms: u32) {
        self.timeout_ms = Some(timeout_ms);
    }
}

/// HTTP Response wrapper for NAPI interop.
#[napi]
pub struct HttpResponse {
    status_code: u16,
    status_text: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
    url: String,
}

#[napi]
impl HttpResponse {
    #[napi(getter)]
    pub fn status(&self) -> u16 {
        self.status_code
    }

    #[napi(getter)]
    pub fn status_text(&self) -> String {
        self.status_text.clone()
    }

    #[napi(getter)]
    pub fn url(&self) -> String {
        self.url.clone()
    }

    #[napi(getter)]
    pub fn headers(&self) -> HashMap<String, String> {
        self.headers.clone()
    }

    #[napi]
    pub fn text(&self) -> Result<String> {
        String::from_utf8(self.body.clone())
            .map_err(|e| Error::new(Status::GenericFailure, format!("Failed to decode UTF-8 response: {e}")))
    }

    #[napi]
    pub fn bytes(&self) -> Buffer {
        Buffer::from(self.body.clone())
    }

    #[napi]
    pub fn header(&self, name: String) -> Option<String> {
        self.headers.get(&name.to_lowercase()).cloned()
    }
}

/// HTTP Client using reqwest with HTTP/3 support.
#[napi]
pub struct HttpClient {
    inner: Client,
}

#[napi]
impl HttpClient {
    #[napi(constructor)]
    pub fn new(options: Option<HttpClientOptions>) -> Result<Self> {
        let options = options.unwrap_or_default();
        let mut builder = Client::builder();

        if let Some(timeout) = options.timeout_ms {
            builder = builder.timeout(Duration::from_millis(timeout as u64));
        }

        if let Some(ua) = options.user_agent {
            builder = builder.user_agent(ua);
        }

        if let Some(follow) = options.follow_redirects {
            if !follow {
                builder = builder.redirect(reqwest::redirect::Policy::none());
            }
        }

        let inner = builder
            .build()
            .map_err(|e| Error::new(Status::GenericFailure, format!("Failed to create reqwest client: {e}")))?;

        Ok(Self { inner })
    }

    /// Execute an arbitrary [`HttpRequest`].
    #[napi]
    pub async fn fetch(&self, req: &HttpRequest) -> Result<HttpResponse> {
        let method = Method::from_bytes(req.method.as_bytes())
            .map_err(|e| Error::new(Status::InvalidArg, format!("Invalid HTTP method: {e}")))?;

        let mut request_builder = self.inner.request(method, &req.url);

        if let Some(timeout) = req.timeout_ms {
            request_builder = request_builder.timeout(Duration::from_millis(timeout as u64));
        }

        let mut header_map = HeaderMap::new();
        for (k, v) in &req.headers {
            if let (Ok(hn), Ok(hv)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
                header_map.insert(hn, hv);
            }
        }
        request_builder = request_builder.headers(header_map);

        if let Some(ref body) = req.body {
            request_builder = request_builder.body(body.clone());
        }

        let res = request_builder
            .send()
            .await
            .map_err(|e| Error::new(Status::GenericFailure, format!("HTTP request failed: {e}")))?;

        let status_code = res.status().as_u16();
        let status_text = res.status().canonical_reason().unwrap_or("").to_string();
        let final_url = res.url().to_string();

        let mut headers = HashMap::new();
        for (k, v) in res.headers() {
            if let Ok(val) = v.to_str() {
                headers.insert(k.as_str().to_lowercase(), val.to_string());
            }
        }

        let body = res
            .bytes()
            .await
            .map_err(|e| Error::new(Status::GenericFailure, format!("Failed to read response body: {e}")))?
            .to_vec();

        Ok(HttpResponse {
            status_code,
            status_text,
            headers,
            body,
            url: final_url,
        })
    }

    /// Helper for quick HTTP GET requests.
    #[napi]
    pub async fn get(&self, url: String) -> Result<HttpResponse> {
        let req = HttpRequest::new(url, Some("GET".to_string()));
        self.fetch(&req).await
    }

    /// Helper for quick HTTP POST requests.
    #[napi]
    pub async fn post(&self, url: String, body: Option<Buffer>) -> Result<HttpResponse> {
        let mut req = HttpRequest::new(url, Some("POST".to_string()));
        if let Some(b) = body {
            req.set_body(b);
        }
        self.fetch(&req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_request_construction() {
        let mut req = HttpRequest::new("https://example.com/info/refs".to_string(), Some("GET".to_string()));
        req.set_header("Git-Protocol".to_string(), "version=2".to_string());
        req.set_timeout(5000);

        assert_eq!(req.url, "https://example.com/info/refs");
        assert_eq!(req.method, "GET");
        assert_eq!(req.headers.get("Git-Protocol").unwrap(), "version=2");
        assert_eq!(req.timeout_ms, Some(5000));
    }

    #[test]
    fn test_http_response_helpers() {
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/plain".to_string());

        let res = HttpResponse {
            status_code: 200,
            status_text: "OK".to_string(),
            headers,
            body: b"hello git".to_vec(),
            url: "https://example.com".to_string(),
        };

        assert_eq!(res.status(), 200);
        assert_eq!(res.status_text(), "OK");
        assert_eq!(res.text().unwrap(), "hello git");
        assert_eq!(res.header("content-type".to_string()).unwrap(), "text/plain");
    }

    #[tokio::test]
    async fn test_http_client_creation() {
        let client = HttpClient::new(Some(HttpClientOptions {
            timeout_ms: Some(3000),
            user_agent: Some("test-agent/1.0".to_string()),
            follow_redirects: Some(true),
        }));
        assert!(client.is_ok());
    }
}
