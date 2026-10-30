//! NAPI bindings for object store network connections and connectors.
//!
//! Provides connector utilities and `ObjectStoreClient` configurations.

use napi_derive::napi;

/// Connector options for ObjectStore network clients.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct ConnectorOptions {
    /// Connection timeout in milliseconds.
    pub timeout_ms: Option<u32>,
    /// User Agent string for HTTP/HTTPS requests.
    pub user_agent: Option<String>,
    /// Proxy URL (e.g. `http://proxy.example.com:8080`).
    pub proxy_url: Option<String>,
    /// Allow insecure HTTP connections.
    pub allow_http: Option<bool>,
    /// Allow invalid/self-signed SSL certificates.
    pub allow_invalid_certificates: Option<bool>,
}

/// A client handle for object store network operations.
#[napi]
pub struct ObjectStoreClient {
    options: ConnectorOptions,
}

#[napi]
impl ObjectStoreClient {
    /// Construct a new `ObjectStoreClient` with the given connector options.
    #[napi(constructor)]
    pub fn new(options: Option<ConnectorOptions>) -> Self {
        Self {
            options: options.unwrap_or_default(),
        }
    }

    /// User agent configured for this client.
    #[napi(getter)]
    pub fn user_agent(&self) -> Option<String> {
        self.options.user_agent.clone()
    }

    /// Proxy URL configured for this client.
    #[napi(getter)]
    pub fn proxy_url(&self) -> Option<String> {
        self.options.proxy_url.clone()
    }

    /// Whether non-TLS HTTP is allowed.
    #[napi(getter)]
    pub fn allow_http(&self) -> bool {
        self.options.allow_http.unwrap_or(false)
    }

    /// Connection timeout in milliseconds.
    #[napi(getter)]
    pub fn timeout_ms(&self) -> Option<u32> {
        self.options.timeout_ms
    }
}
