//! NAPI bindings for `ObjectStoreClient` and client network configurations (HTTP / SSH transport options).

use std::time::Duration;
use napi_derive::napi;
use object_store::ClientOptions;

/// Options for configuring the HTTP / network client used by Object Stores and Transports.
#[napi(object)]
#[derive(Default)]
pub struct ObjectStoreClientOptions {
    /// Allow plain HTTP connections (default: false).
    pub allow_http: Option<bool>,
    /// Allow invalid SSL/TLS certificates (default: false).
    pub allow_invalid_certificates: Option<bool>,
    /// Connection timeout in milliseconds.
    pub connect_timeout_ms: Option<u32>,
    /// Request timeout in milliseconds.
    pub timeout_ms: Option<u32>,
    /// Read timeout in milliseconds.
    pub read_timeout_ms: Option<u32>,
    /// Proxy URL for network requests.
    pub proxy_url: Option<String>,
    /// Custom User-Agent header value.
    pub user_agent: Option<String>,
    /// Force HTTP/1.1 only.
    pub http1_only: Option<bool>,
    /// Force HTTP/2 only.
    pub http2_only: Option<bool>,
}

/// Client handle managing HTTP/HTTPS/SSH transport options and connection settings.
#[napi]
pub struct ObjectStoreClient {
    pub(crate) options: ClientOptions,
}

#[napi]
impl ObjectStoreClient {
    /// Create a new ObjectStoreClient with the given options.
    #[napi(constructor)]
    pub fn new(options: Option<ObjectStoreClientOptions>) -> napi::Result<Self> {
        let mut client_opts = ClientOptions::new();
        if let Some(opts) = options {
            if let Some(allow_http) = opts.allow_http {
                client_opts = client_opts.with_allow_http(allow_http);
            }
            if let Some(allow_invalid) = opts.allow_invalid_certificates {
                client_opts = client_opts.with_allow_invalid_certificates(allow_invalid);
            }
            if let Some(conn_timeout) = opts.connect_timeout_ms {
                client_opts = client_opts.with_connect_timeout(Duration::from_millis(conn_timeout as u64));
            }
            if let Some(timeout) = opts.timeout_ms {
                client_opts = client_opts.with_timeout(Duration::from_millis(timeout as u64));
            }
            if let Some(read_timeout) = opts.read_timeout_ms {
                client_opts = client_opts.with_read_timeout(Duration::from_millis(read_timeout as u64));
            }
            if let Some(proxy_url) = opts.proxy_url {
                client_opts = client_opts.with_proxy_url(proxy_url);
            }
            if let Some(user_agent) = opts.user_agent {
                if let Ok(header_val) = reqwest::header::HeaderValue::from_str(&user_agent) {
                    client_opts = client_opts.with_user_agent(header_val);
                }
            }
            if let Some(true) = opts.http1_only {
                client_opts = client_opts.with_http1_only();
            }
            if let Some(true) = opts.http2_only {
                client_opts = client_opts.with_http2_only();
            }
        }
        Ok(ObjectStoreClient {
            options: client_opts,
        })
    }

    /// Return whether HTTP connections are permitted.
    #[napi(getter)]
    pub fn allow_http(&self) -> bool {
        self.options
            .get_config_value(&object_store::ClientConfigKey::AllowHttp)
            .and_then(|v| v.parse().ok())
            .unwrap_or(false)
    }

    /// Return whether invalid certificates are permitted.
    #[napi(getter)]
    pub fn allow_invalid_certificates(&self) -> bool {
        self.options
            .get_config_value(&object_store::ClientConfigKey::AllowInvalidCertificates)
            .and_then(|v| v.parse().ok())
            .unwrap_or(false)
    }

    /// Return configured proxy URL if set.
    #[napi(getter)]
    pub fn proxy_url(&self) -> Option<String> {
        self.options.get_config_value(&object_store::ClientConfigKey::ProxyUrl)
    }
}
