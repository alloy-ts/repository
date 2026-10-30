//! NAPI bindings for HTTP connector and connection builder configuration (`src/object_store/connect.rs`).

use std::time::Duration;
use napi_derive::napi;

/// Options for configuring an HTTP connector.
#[napi(object)]
#[derive(Default, Debug, Clone)]
pub struct HttpConnectorOptions {
    /// Connection timeout in milliseconds.
    pub connect_timeout_ms: Option<u32>,
    /// Keep-alive duration in milliseconds.
    pub keepalive_ms: Option<u32>,
    /// Whether Nagle's algorithm is disabled (`TCP_NODELAY`). Defaults to true.
    pub nodelay: Option<bool>,
    /// HTTP proxy URL if proxying is enabled.
    pub proxy_url: Option<String>,
    /// Unix domain socket path (Unix platforms only).
    pub unix_socket: Option<String>,
}

/// Managed HTTP connector for custom network connections.
#[napi]
pub struct ObjectStoreConnector {
    connect_timeout: Option<Duration>,
    keepalive: Option<Duration>,
    nodelay: bool,
    proxy_url: Option<String>,
    unix_socket: Option<String>,
}

#[napi]
impl ObjectStoreConnector {
    /// Create a new [`ObjectStoreConnector`] with options.
    #[napi(constructor)]
    pub fn new(options: Option<HttpConnectorOptions>) -> Self {
        let opts = options.unwrap_or_default();
        Self {
            connect_timeout: opts.connect_timeout_ms.map(|ms| Duration::from_millis(ms as u64)),
            keepalive: opts.keepalive_ms.map(|ms| Duration::from_millis(ms as u64)),
            nodelay: opts.nodelay.unwrap_or(true),
            proxy_url: opts.proxy_url,
            unix_socket: opts.unix_socket,
        }
    }

    /// True if `TCP_NODELAY` is enabled.
    #[napi(getter)]
    pub fn nodelay(&self) -> bool {
        self.nodelay
    }

    /// Proxy URL string if configured.
    #[napi(getter)]
    pub fn proxy_url(&self) -> Option<String> {
        self.proxy_url.clone()
    }

    /// Unix domain socket path if configured.
    #[napi(getter)]
    pub fn unix_socket(&self) -> Option<String> {
        self.unix_socket.clone()
    }

    /// Connect timeout in milliseconds, if configured.
    #[napi(getter)]
    pub fn connect_timeout_ms(&self) -> Option<u32> {
        self.connect_timeout.map(|d| d.as_millis() as u32)
    }

    /// Keepalive duration in milliseconds, if configured.
    #[napi(getter)]
    pub fn keepalive_ms(&self) -> Option<u32> {
        self.keepalive.map(|d| d.as_millis() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connector_options() {
        let connector = ObjectStoreConnector::new(Some(HttpConnectorOptions {
            connect_timeout_ms: Some(5000),
            keepalive_ms: Some(30000),
            nodelay: Some(true),
            proxy_url: Some("http://proxy.local:8080".to_string()),
            unix_socket: None,
        }));

        assert_eq!(connector.connect_timeout_ms(), Some(5000));
        assert_eq!(connector.keepalive_ms(), Some(30000));
        assert!(connector.nodelay());
        assert_eq!(connector.proxy_url(), Some("http://proxy.local:8080".to_string()));
    }
}
