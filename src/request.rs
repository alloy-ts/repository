//! NAPI bindings for HTTP and network requests over HTTP and SSH transports.

use std::collections::HashMap;
use std::time::Duration;

use napi::bindgen_prelude::*;
use napi_derive::napi;

use crate::error::err_str;

/// Options for an HTTP request executed via [`GitRequestClient`].
#[napi(object)]
#[derive(Default)]
pub struct GitHttpRequest {
    /// Target URL.
    pub url: String,
    /// HTTP method (`"GET"`, `"POST"`, etc.). Defaults to `"GET"`.
    pub method: Option<String>,
    /// Optional HTTP request headers.
    pub headers: Option<HashMap<String, String>>,
    /// Optional binary body payload.
    pub body: Option<Buffer>,
    /// Overall request timeout in milliseconds.
    pub timeout_ms: Option<u32>,
    /// Allow non-TLS HTTP URLs.
    pub allow_http: Option<bool>,
    /// Accept invalid/self-signed TLS certificates.
    pub allow_invalid_certificates: Option<bool>,
}

/// Response returned from an HTTP request execution.
#[napi(object)]
pub struct GitHttpResponse {
    /// HTTP status code (e.g. 200, 404).
    pub status_code: u16,
    /// Response headers map.
    pub headers: HashMap<String, String>,
    /// Binary body contents.
    pub body: Buffer,
}

/// Client for executing HTTP and network transport requests.
#[napi]
#[derive(Clone)]
pub struct GitRequestClient {
    user_agent: String,
}

#[napi]
impl GitRequestClient {
    /// Create a new request client with an optional custom User-Agent.
    #[napi(constructor)]
    pub fn new(user_agent: Option<String>) -> Self {
        GitRequestClient {
            user_agent: user_agent.unwrap_or_else(|| "repository-client/0.1.0".to_string()),
        }
    }

    /// Execute an async HTTP request and return the response Promise.
    #[napi]
    pub async fn execute_http(&self, req: GitHttpRequest) -> Result<GitHttpResponse> {
        let method_str = req.method.as_deref().unwrap_or("GET").to_uppercase();
        let method = match method_str.as_str() {
            "GET" => reqwest::Method::GET,
            "POST" => reqwest::Method::POST,
            "PUT" => reqwest::Method::PUT,
            "HEAD" => reqwest::Method::HEAD,
            "DELETE" => reqwest::Method::DELETE,
            other => {
                return Err(err_str(
                    "execute_http",
                    format!("unsupported HTTP method '{other}'"),
                ));
            }
        };

        let mut builder = reqwest::ClientBuilder::new().user_agent(&self.user_agent);

        if req.allow_http == Some(true) {
            builder = builder.https_only(false);
        }
        if req.allow_invalid_certificates == Some(true) {
            builder = builder.danger_accept_invalid_certs(true);
        }
        if let Some(timeout_ms) = req.timeout_ms {
            builder = builder.timeout(Duration::from_millis(timeout_ms as u64));
        }

        let client = builder
            .build()
            .map_err(|e| err_str("execute_http", format!("Failed to build HTTP client: {e:?}")))?;

        let mut request = client.request(method, &req.url);

        if let Some(headers) = req.headers {
            for (k, v) in headers {
                request = request.header(k, v);
            }
        }

        if let Some(body) = req.body {
            request = request.body(body.to_vec());
        }

        let response = request
            .send()
            .await
            .map_err(|e| err_str("execute_http", format!("HTTP request failed: {e:?}")))?;

        let status_code = response.status().as_u16();

        let mut res_headers = HashMap::new();
        for (k, v) in response.headers().iter() {
            if let Ok(val_str) = v.to_str() {
                res_headers.insert(k.to_string(), val_str.to_string());
            }
        }

        let body_bytes = response
            .bytes()
            .await
            .map_err(|e| err_str("execute_http", format!("Failed to read response body: {e:?}")))?
            .to_vec();

        Ok(GitHttpResponse {
            status_code,
            headers: res_headers,
            body: Buffer::from(body_bytes),
        })
    }
}

/// Execute an HTTP request asynchronously with default client settings.
#[napi]
pub async fn send_git_http_request(req: GitHttpRequest) -> Result<GitHttpResponse> {
    let client = GitRequestClient::new(None);
    client.execute_http(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_request_client_basic() {
        let client = GitRequestClient::new(Some("test-agent".to_string()));
        assert_eq!(client.user_agent, "test-agent");
    }
}
