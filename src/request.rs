//! NAPI bindings for HTTP (reqwest) and SSH request / transport execution.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::collections::HashMap;

/// Transport channel variant supported for remote git operations.
#[napi]
pub enum TransportChannel {
    Http,
    Https,
    Ssh,
    Git,
    File,
}

/// HTTP request configuration for git protocol wire operations using reqwest.
#[napi]
pub struct HttpRequest {
    url: String,
    method: String,
    headers: HashMap<String, String>,
    body: Option<Vec<u8>>,
    timeout_ms: Option<u32>,
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

    /// Add a header to the request.
    #[napi]
    pub fn set_header(&mut self, key: String, value: String) {
        self.headers.insert(key, value);
    }

    /// Set request timeout in milliseconds.
    #[napi]
    pub fn set_timeout_ms(&mut self, timeout_ms: u32) {
        self.timeout_ms = Some(timeout_ms);
    }

    /// Set binary payload body.
    #[napi]
    pub fn set_body(&mut self, body: Buffer) {
        self.body = Some(body.to_vec());
    }

    /// Prepare headers for Git smart HTTP service (e.g. `upload-pack` or `receive-pack`).
    #[napi]
    pub fn prepare_git_service_headers(&mut self, service: String) {
        let content_type = format!("application/x-git-{service}-request");
        let accept = format!("application/x-git-{service}-result");
        self.headers.insert("Content-Type".to_string(), content_type);
        self.headers.insert("Accept".to_string(), accept);
        self.headers.insert("User-Agent".to_string(), "git/2.40.0 (gix)".to_string());
    }

    /// Execute the HTTP request asynchronously via reqwest.
    #[napi]
    pub async fn send(&self) -> napi::Result<HttpResponse> {
        let mut builder = match self.method.to_uppercase().as_str() {
            "POST" => reqwest::Client::new().post(&self.url),
            "PUT" => reqwest::Client::new().put(&self.url),
            "DELETE" => reqwest::Client::new().delete(&self.url),
            "HEAD" => reqwest::Client::new().head(&self.url),
            _ => reqwest::Client::new().get(&self.url),
        };

        for (k, v) in &self.headers {
            builder = builder.header(k, v);
        }

        if let Some(timeout) = self.timeout_ms {
            builder = builder.timeout(std::time::Duration::from_millis(timeout as u64));
        }

        if let Some(ref body) = self.body {
            builder = builder.body(body.clone());
        }

        let resp = builder
            .send()
            .await
            .map_err(|e| napi::Error::from_reason(format!("HTTP request failed: {e}")))?;

        let status = resp.status().as_u16();
        let mut resp_headers = HashMap::new();
        for (k, v) in resp.headers() {
            if let Ok(val) = v.to_str() {
                resp_headers.insert(k.as_str().to_string(), val.to_string());
            }
        }

        let body_bytes = resp
            .bytes()
            .await
            .map_err(|e| napi::Error::from_reason(format!("Failed to read response body: {e}")))?;

        Ok(HttpResponse {
            status,
            headers: resp_headers,
            body: body_bytes.to_vec(),
        })
    }
}

/// HTTP response received from a git remote request.
#[napi]
pub struct HttpResponse {
    pub status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

#[napi]
impl HttpResponse {
    #[napi(getter)]
    pub fn get_status(&self) -> u16 {
        self.status
    }

    #[napi(getter)]
    pub fn get_headers(&self) -> HashMap<String, String> {
        self.headers.clone()
    }

    #[napi]
    pub fn get_header(&self, name: String) -> Option<String> {
        self.headers.get(&name.to_lowercase()).cloned()
    }

    #[napi]
    pub fn body_bytes(&self) -> Buffer {
        Buffer::from(self.body.clone())
    }

    #[napi]
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// SSH channel execution parameters for remote git commands (e.g., `git-upload-pack`).
#[napi]
pub struct SshRequest {
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
    pub command: String,
    pub key_path: Option<String>,
}

#[napi]
impl SshRequest {
    #[napi(constructor)]
    pub fn new(host: String, command: String, port: Option<u16>, user: Option<String>, key_path: Option<String>) -> Self {
        Self {
            host,
            port: port.unwrap_or(22),
            user,
            command,
            key_path,
        }
    }

    /// Construct a system SSH command string for subprocess execution.
    #[napi]
    pub fn to_command_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if self.port != 22 {
            args.push("-p".to_string());
            args.push(self.port.to_string());
        }
        if let Some(ref key) = self.key_path {
            args.push("-i".to_string());
            args.push(key.clone());
        }

        let destination = match &self.user {
            Some(u) => format!("{u}@{}", self.host),
            None => self.host.clone(),
        };
        args.push(destination);
        args.push(self.command.clone());

        args
    }
}
