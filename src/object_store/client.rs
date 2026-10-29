use std::time::Duration;

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use crate::error::err;

/// Which HTTP version(s) the client may negotiate.
#[napi(string_enum)]
pub enum HttpVersion {
    /// HTTP/1 only (the `object_store` default).
    Http1Only,
    /// HTTP/2 only.
    Http2Only,
    /// HTTP/1 or HTTP/2 (negotiated; HTTP/2 used if supported).
    Either,
}

/// A single `ClientConfigKey` → value entry, applied verbatim via `ClientOptions::with_config`.
///
/// Lets callers set options not given a dedicated field on [`ObjectStoreClient`] (e.g.
/// `user_agent`, `proxy_ca_cert`, `dns_timeout`, …). See `object_store`'s `ClientConfigKey` for
/// the accepted key names.
#[napi(object)]
pub struct ConfigEntry {
    /// The `ClientConfigKey` name (e.g. `"user_agent"`, `"timeout"`).
    pub key: String,
    /// The string value (e.g. `"my-agent/1.0"`, `"30s"`).
    pub value: String,
}

/// HTTP client configuration for remote object stores, mirroring
/// `object_store::client::ClientOptions`.
///
/// Build one as a plain JS object and pass it to
/// [`GenericObjectStore`](crate::object_store::base::GenericObjectStore)'s constructor to configure
/// the HTTP/HTTPS backend. All fields are optional; `None` keeps `object_store`'s default.
///
/// This is the single connection-config surface for the crate — it supersedes the former
/// separate `ObjectStoreConnector` builder, folding every granular knob (proxy scheme / CA,
/// custom root CAs, TLS verification toggles, HTTP/2 keep-alive, pool sizing, DNS randomization)
/// into one plain object.
#[napi(object)]
pub struct ObjectStoreClient {
    /// Allow (`true`) plain-HTTP connections. Default `false`.
    pub allow_http: Option<bool>,
    /// Accept invalid TLS certificates (testing only).
    pub allow_invalid_certificates: Option<bool>,
    /// Disable the operating system's trust store; only certs supplied via `root_certificates` /
    /// `root_certificate_files` are then trusted. Default `false`.
    pub disable_system_certificates: Option<bool>,
    /// `User-Agent` header sent on every request.
    pub user_agent: Option<String>,
    /// Overall request timeout in seconds.
    pub timeout: Option<f64>,
    /// Connect-phase timeout in seconds.
    pub connect_timeout: Option<f64>,
    /// Per-read timeout in seconds.
    pub read_timeout: Option<f64>,
    /// HTTP proxy URL. May embed scheme and basic auth (`"https://user:pass@proxy:3128"`);
    /// `object_store` passes it verbatim to reqwest's `Proxy::all`.
    pub proxy_url: Option<String>,
    /// Hosts that bypass the proxy (comma/space separated), mirroring curl's `NO_PROXY`.
    pub proxy_excludes: Option<String>,
    /// PEM CA certificate used to validate the proxy's TLS endpoint.
    pub proxy_ca_certificate: Option<String>,
    /// Default `Content-Type` for uploads.
    pub default_content_type: Option<String>,
    /// HTTP version negotiation policy.
    pub http_version: Option<HttpVersion>,
    /// PEM-encoded root certificates appended to the trust store (in addition to, or instead of
    /// when `disable_system_certificates` is set, the system store).
    pub root_certificates: Option<Vec<Buffer>>,
    /// Paths to PEM certificate bundle files whose every certificate is appended to the trust store.
    pub root_certificate_files: Option<Vec<String>>,
    /// Keep-alive idle timeout for pooled connections, in seconds.
    pub pool_idle_timeout: Option<f64>,
    /// Maximum number of idle connections kept per host (`None` = no limit).
    pub pool_max_idle_per_host: Option<f64>,
    /// HTTP/2 keep-alive ping interval, in seconds.
    pub http2_keep_alive_interval: Option<f64>,
    /// Timeout for an HTTP/2 keep-alive ping acknowledgement, in seconds.
    pub http2_keep_alive_timeout: Option<f64>,
    /// Send HTTP/2 keep-alive pings even when no streams are active.
    pub http2_keep_alive_while_idle: Option<bool>,
    /// Maximum HTTP/2 frame size, in bytes.
    pub http2_max_frame_size: Option<f64>,
    /// Randomize the order of addresses returned by DNS (spreads connections across servers).
    /// Default `true`.
    pub randomize_addresses: Option<bool>,
    /// Additional raw `ClientConfigKey` entries applied verbatim (escape hatch for any option
    /// without a dedicated field above).
    pub extra_config: Option<Vec<ConfigEntry>>,
}

impl ObjectStoreClient {
    /// Build the `object_store` `ClientOptions` from this config object.
    pub(crate) fn to_client_options(&self) -> napi::Result<::object_store::client::ClientOptions> {
        let mut opts = ::object_store::client::ClientOptions::new();

        if let Some(true) = self.allow_http {
            opts = opts.with_allow_http(true);
        }
        if let Some(true) = self.allow_invalid_certificates {
            opts = opts.with_allow_invalid_certificates(true);
        }
        if let Some(true) = self.disable_system_certificates {
            opts = opts.with_no_system_certificates(true);
        }
        if let Some(agent) = &self.user_agent {
            opts = opts.with_config(
                ::object_store::client::ClientConfigKey::UserAgent,
                agent.clone(),
            );
        }
        if let Some(s) = self.timeout {
            opts = opts.with_timeout(Duration::from_secs_f64(s));
        }
        if let Some(s) = self.connect_timeout {
            opts = opts.with_connect_timeout(Duration::from_secs_f64(s));
        }
        if let Some(s) = self.read_timeout {
            opts = opts.with_read_timeout(Duration::from_secs_f64(s));
        }
        if let Some(url) = &self.proxy_url {
            opts = opts.with_proxy_url(url.clone());
        }
        if let Some(excludes) = &self.proxy_excludes {
            opts = opts.with_proxy_excludes(excludes.clone());
        }
        if let Some(ca) = &self.proxy_ca_certificate {
            opts = opts.with_proxy_ca_certificate(ca.clone());
        }
        if let Some(mime) = &self.default_content_type {
            opts = opts.with_default_content_type(mime.clone());
        }
        match self.http_version {
            Some(HttpVersion::Http1Only) => opts = opts.with_http1_only(),
            Some(HttpVersion::Http2Only) => opts = opts.with_http2_only(),
            Some(HttpVersion::Either) => opts = opts.with_allow_http2(),
            None => {}
        }
        if let Some(certs) = &self.root_certificates {
            for pem in certs {
                let cert = ::object_store::client::Certificate::from_pem(pem)
                    .map_err(|e| err("root_certificates", e))?;
                opts = opts.with_root_certificate(cert);
            }
        }
        if let Some(files) = &self.root_certificate_files {
            for path in files {
                let bytes = std::fs::read(path).map_err(|e| err("root_certificate_files", e))?;
                let certs = ::object_store::client::Certificate::from_pem_bundle(&bytes)
                    .map_err(|e| err("root_certificate_files", e))?;
                for cert in certs {
                    opts = opts.with_root_certificate(cert);
                }
            }
        }
        if let Some(s) = self.pool_idle_timeout {
            opts = opts.with_pool_idle_timeout(Duration::from_secs_f64(s));
        }
        if let Some(m) = self.pool_max_idle_per_host {
            opts = opts.with_pool_max_idle_per_host(m as usize);
        }
        if let Some(s) = self.http2_keep_alive_interval {
            opts = opts.with_http2_keep_alive_interval(Duration::from_secs_f64(s));
        }
        if let Some(s) = self.http2_keep_alive_timeout {
            opts = opts.with_http2_keep_alive_timeout(Duration::from_secs_f64(s));
        }
        if let Some(true) = self.http2_keep_alive_while_idle {
            opts = opts.with_http2_keep_alive_while_idle();
        }
        if let Some(size) = self.http2_max_frame_size {
            opts = opts.with_http2_max_frame_size(size as u32);
        }
        if let Some(randomize) = self.randomize_addresses {
            opts = opts.with_config(
                ::object_store::client::ClientConfigKey::RandomizeAddresses,
                randomize.to_string(),
            );
        }
        if let Some(entries) = &self.extra_config {
            for entry in entries {
                let key = entry
                    .key
                    .parse::<::object_store::client::ClientConfigKey>()
                    .map_err(|e| {
                        napi::Error::from_reason(format!("invalid client config key: {e}"))
                    })?;
                opts = opts.with_config(key, entry.value.clone());
            }
        }

        Ok(opts)
    }
}
