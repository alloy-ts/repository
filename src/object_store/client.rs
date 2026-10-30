//! NAPI bindings for ObjectStore client configuration and transport connectors (HTTP/SSH).

use std::str::FromStr;
use std::time::Duration;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use object_store::client::{ClientConfigKey, ClientOptions};

use crate::error::err_str;

/// SSH transport connector configuration options.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct ObjectStoreSshConfig {
    /// Target host name or IP address.
    pub host: Option<String>,
    /// SSH port (defaults to 22 when omitted).
    pub port: Option<u32>,
    /// Remote user name.
    pub user: Option<String>,
    /// Path to local SSH private key file.
    pub key_path: Option<String>,
    /// Path to local SSH known_hosts file.
    pub known_hosts_path: Option<String>,
    /// Passphrase for encrypted private key.
    pub passphrase: Option<String>,
}

/// Options for configuring an [`ObjectStoreClient`].
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct ObjectStoreClientConfig {
    /// Allow non-TLS (http://) HTTP connections.
    pub allow_http: Option<bool>,
    /// Skip certificate validation on HTTPS connections.
    pub allow_invalid_certificates: Option<bool>,
    /// Disable certificate validation using OS facility.
    pub disable_system_certificates: Option<bool>,
    /// Connect phase timeout in milliseconds.
    pub connect_timeout_ms: Option<u32>,
    /// Read timeout in milliseconds.
    pub read_timeout_ms: Option<u32>,
    /// Overall request timeout in milliseconds.
    pub timeout_ms: Option<u32>,
    /// User-Agent header value.
    pub user_agent: Option<String>,
    /// HTTP proxy URL.
    pub proxy_url: Option<String>,
    /// PEM-formatted CA certificate for proxy.
    pub proxy_ca_certificate: Option<String>,
    /// Hosts to exclude from proxying.
    pub proxy_excludes: Option<String>,
    /// Default Content-Type for uploaded objects.
    pub default_content_type: Option<String>,
    /// Force HTTP/1.1 connections.
    pub http1_only: Option<bool>,
    /// Force HTTP/2 connections.
    pub http2_only: Option<bool>,
    /// Randomize DNS address resolution order.
    pub randomize_addresses: Option<bool>,
    /// Idle connection pool timeout in milliseconds.
    pub pool_idle_timeout_ms: Option<u32>,
    /// Maximum idle connections per host.
    pub pool_max_idle_per_host: Option<u32>,
    /// SSH transport connector settings.
    pub ssh: Option<ObjectStoreSshConfig>,
}

/// Client configuration and connector handle for HTTP/reqwest and SSH transports.
#[napi]
#[derive(Clone)]
pub struct ObjectStoreClient {
    options: ClientOptions,
    ssh_config: Option<ObjectStoreSshConfig>,
}

#[napi]
impl ObjectStoreClient {
    /// Create a new `ObjectStoreClient` with default or user-specified configuration.
    #[napi(constructor)]
    pub fn new(config: Option<ObjectStoreClientConfig>) -> Result<Self> {
        let mut client_opts = ClientOptions::new();
        let mut ssh_config = None;

        if let Some(cfg) = config {
            if let Some(allow) = cfg.allow_http {
                client_opts = client_opts.with_allow_http(allow);
            }
            if let Some(allow_invalid) = cfg.allow_invalid_certificates {
                client_opts = client_opts.with_allow_invalid_certificates(allow_invalid);
            }
            if let Some(no_sys) = cfg.disable_system_certificates {
                client_opts = client_opts.with_no_system_certificates(no_sys);
            }
            if let Some(ms) = cfg.connect_timeout_ms {
                client_opts = client_opts.with_connect_timeout(Duration::from_millis(ms as u64));
            }
            if let Some(ms) = cfg.read_timeout_ms {
                client_opts = client_opts.with_read_timeout(Duration::from_millis(ms as u64));
            }
            if let Some(ms) = cfg.timeout_ms {
                client_opts = client_opts.with_timeout(Duration::from_millis(ms as u64));
            }
            if let Some(ua) = cfg.user_agent {
                client_opts = client_opts.with_config(ClientConfigKey::UserAgent, ua);
            }
            if let Some(proxy) = cfg.proxy_url {
                client_opts = client_opts.with_proxy_url(proxy);
            }
            if let Some(ca) = cfg.proxy_ca_certificate {
                client_opts = client_opts.with_proxy_ca_certificate(ca);
            }
            if let Some(ex) = cfg.proxy_excludes {
                client_opts = client_opts.with_proxy_excludes(ex);
            }
            if let Some(ct) = cfg.default_content_type {
                client_opts = client_opts.with_default_content_type(ct);
            }
            if let Some(h1) = cfg.http1_only {
                if h1 {
                    client_opts = client_opts.with_http1_only();
                }
            }
            if let Some(h2) = cfg.http2_only {
                if h2 {
                    client_opts = client_opts.with_http2_only();
                }
            }
            if let Some(idle_ms) = cfg.pool_idle_timeout_ms {
                client_opts = client_opts.with_pool_idle_timeout(Duration::from_millis(idle_ms as u64));
            }
            if let Some(max_idle) = cfg.pool_max_idle_per_host {
                client_opts = client_opts.with_pool_max_idle_per_host(max_idle as usize);
            }
            ssh_config = cfg.ssh;
        }

        Ok(ObjectStoreClient {
            options: client_opts,
            ssh_config,
        })
    }

    /// Set an option by string key and string value (e.g. `"allow_http"`, `"proxy_url"`).
    #[napi]
    pub fn set_config(&mut self, key: String, value: String) -> Result<()> {
        let parsed_key = ClientConfigKey::from_str(&key)
            .map_err(|e| err_str("set_config", format!("Invalid config key '{key}': {e:?}")))?;
        self.options = self.options.clone().with_config(parsed_key, value);
        Ok(())
    }

    /// Get an option value by string key.
    #[napi]
    pub fn get_config_value(&self, key: String) -> Result<Option<String>> {
        let parsed_key = ClientConfigKey::from_str(&key)
            .map_err(|e| err_str("get_config_value", format!("Invalid config key '{key}': {e:?}")))?;
        Ok(self.options.get_config_value(&parsed_key))
    }

    /// Whether non-TLS HTTP connections are allowed.
    #[napi]
    pub fn set_allow_http(&mut self, allow: bool) {
        self.options = self.options.clone().with_allow_http(allow);
    }

    /// Allow connections with invalid or self-signed TLS certificates.
    #[napi]
    pub fn set_allow_invalid_certificates(&mut self, allow: bool) {
        self.options = self.options.clone().with_allow_invalid_certificates(allow);
    }

    /// Set HTTP User-Agent string.
    #[napi]
    pub fn set_user_agent(&mut self, user_agent: String) {
        self.options = self.options.clone().with_config(ClientConfigKey::UserAgent, user_agent);
    }

    /// Set HTTP Proxy URL.
    #[napi]
    pub fn set_proxy_url(&mut self, proxy_url: String) {
        self.options = self.options.clone().with_proxy_url(proxy_url);
    }

    /// Set request timeout in milliseconds.
    #[napi]
    pub fn set_timeout_ms(&mut self, timeout_ms: u32) {
        self.options = self.options.clone().with_timeout(Duration::from_millis(timeout_ms as u64));
    }

    /// Set connect timeout in milliseconds.
    #[napi]
    pub fn set_connect_timeout_ms(&mut self, timeout_ms: u32) {
        self.options = self.options.clone().with_connect_timeout(Duration::from_millis(timeout_ms as u64));
    }

    /// Set read timeout in milliseconds.
    #[napi]
    pub fn set_read_timeout_ms(&mut self, timeout_ms: u32) {
        self.options = self.options.clone().with_read_timeout(Duration::from_millis(timeout_ms as u64));
    }

    /// Get current SSH connector options.
    #[napi]
    pub fn get_ssh_config(&self) -> Option<ObjectStoreSshConfig> {
        self.ssh_config.clone()
    }

    /// Set current SSH connector options.
    #[napi]
    pub fn set_ssh_config(&mut self, config: Option<ObjectStoreSshConfig>) {
        self.ssh_config = config;
    }

    /// Generate system SSH command arguments based on configured SSH options.
    #[napi]
    pub fn get_ssh_command_prefix(&self) -> Result<Vec<String>> {
        let ssh = self.ssh_config.as_ref().ok_or_else(|| {
            err_str("get_ssh_command_prefix", "SSH configuration is not set")
        })?;

        let host = ssh.host.as_deref().ok_or_else(|| {
            err_str("get_ssh_command_prefix", "SSH host is required")
        })?;

        let mut args = vec!["ssh".to_string()];

        if let Some(port) = ssh.port {
            args.push("-p".to_string());
            args.push(port.to_string());
        }

        if let Some(key_path) = &ssh.key_path {
            args.push("-i".to_string());
            args.push(key_path.clone());
        }

        if let Some(known_hosts) = &ssh.known_hosts_path {
            args.push("-o".to_string());
            args.push(format!("UserKnownHostsFile={known_hosts}"));
        }

        let target = if let Some(user) = &ssh.user {
            format!("{user}@{host}")
        } else {
            host.to_string()
        };
        args.push(target);

        Ok(args)
    }

    /// Execute a command over the configured SSH transport asynchronously.
    #[napi]
    pub async fn execute_ssh_command(&self, command: String) -> Result<String> {
        let mut prefix = self.get_ssh_command_prefix()?;
        if prefix.is_empty() {
            return Err(err_str("execute_ssh_command", "Invalid SSH command prefix"));
        }

        let binary = prefix.remove(0);
        prefix.push(command);

        let output = tokio::process::Command::new(&binary)
            .args(&prefix)
            .output()
            .await
            .map_err(|e| err_str("execute_ssh_command", format!("Failed to run SSH process: {e:?}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(err_str(
                "execute_ssh_command",
                format!("SSH command exited with status {}: {stderr}", output.status),
            ));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Build the underlying reqwest HTTP client to ensure options are valid.
    #[napi]
    pub fn build_http_client_ok(&self) -> Result<bool> {
        let opts = &self.options;
        let mut builder = reqwest::ClientBuilder::new();

        if let Some(ua) = opts.get_config_value(&ClientConfigKey::UserAgent) {
            builder = builder.user_agent(ua);
        }
        if let Some(proxy_url) = opts.get_config_value(&ClientConfigKey::ProxyUrl) {
            let proxy = reqwest::Proxy::all(proxy_url)
                .map_err(|e| err_str("build_http_client_ok", format!("Invalid proxy URL: {e:?}")))?;
            builder = builder.proxy(proxy);
        }
        if let Some(allow_http) = opts.get_config_value(&ClientConfigKey::AllowHttp) {
            if allow_http == "true" {
                builder = builder.https_only(false);
            }
        }
        if let Some(allow_invalid) = opts.get_config_value(&ClientConfigKey::AllowInvalidCertificates) {
            if allow_invalid == "true" {
                builder = builder.danger_accept_invalid_certs(true);
            }
        }

        builder
            .build()
            .map(|_| true)
            .map_err(|e| err_str("build_http_client_ok", format!("Failed to build reqwest client: {e:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_options_basic() {
        let mut client = ObjectStoreClient::new(Some(ObjectStoreClientConfig {
            allow_http: Some(true),
            user_agent: Some("custom-agent/1.0".to_string()),
            timeout_ms: Some(5000),
            ssh: Some(ObjectStoreSshConfig {
                host: Some("example.com".into()),
                port: Some(2222),
                user: Some("git".into()),
                ..Default::default()
            }),
            ..Default::default()
        }))
        .unwrap();

        assert_eq!(client.get_config_value("allow_http".into()).unwrap(), Some("true".into()));
        assert_eq!(client.get_config_value("user_agent".into()).unwrap(), Some("custom-agent/1.0".into()));

        client.set_config("connect_timeout".into(), "10 seconds".into()).unwrap();
        assert!(client.get_config_value("connect_timeout".into()).unwrap().is_some());

        let ssh = client.get_ssh_config().unwrap();
        assert_eq!(ssh.host.as_deref(), Some("example.com"));
        assert_eq!(ssh.port, Some(2222));

        let ssh_prefix = client.get_ssh_command_prefix().unwrap();
        assert_eq!(ssh_prefix, vec!["ssh", "-p", "2222", "git@example.com"]);

        assert!(client.build_http_client_ok().unwrap());
    }
}
