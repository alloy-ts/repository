//! NAPI bindings for [`ObjectStoreClient`], providing HTTP (reqwest) and SSH transport connectors,
//! configuration key management, and remote object store operations over network protocols.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;
use reqwest::Client as ReqwestClient;

use crate::error::err_str;

/// Configuration options for initializing an [`ObjectStoreClient`].
#[napi(object)]
#[derive(Default, Clone)]
pub struct ObjectStoreClientOptions {
    /// Allow non-TLS (http://) connections. Defaults to `false`.
    pub allow_http: Option<bool>,
    /// Allow invalid SSL/TLS certificates. Defaults to `false`.
    pub allow_invalid_certificates: Option<bool>,
    /// Disable system certificate store. Defaults to `false`.
    pub disable_system_certificates: Option<bool>,
    /// Connection timeout in milliseconds.
    pub connect_timeout_ms: Option<u32>,
    /// Overall request timeout in milliseconds.
    pub timeout_ms: Option<u32>,
    /// Read timeout in milliseconds.
    pub read_timeout_ms: Option<u32>,
    /// User-Agent header string.
    pub user_agent: Option<String>,
    /// Proxy URL (e.g., `http://proxy.example.com:8080`).
    pub proxy_url: Option<String>,
    /// PEM-encoded CA certificate for proxy.
    pub proxy_ca_certificate: Option<String>,
    /// Hosts to exclude from proxy.
    pub proxy_excludes: Option<String>,
    /// Default Content-Type header for uploads.
    pub default_content_type: Option<String>,
    /// Transport protocol: `"http"` (default), `"ssh"`, or `"auto"`.
    pub transport_protocol: Option<String>,
    /// Path to SSH private key file.
    pub ssh_key_path: Option<String>,
    /// SSH username.
    pub ssh_user: Option<String>,
    /// Passphrase for SSH private key.
    pub ssh_passphrase: Option<String>,
}

/// NAPI binding for an object store client supporting HTTP (reqwest) and SSH transports.
#[napi]
pub struct ObjectStoreClient {
    config: Arc<Mutex<HashMap<String, String>>>,
    client: Arc<Mutex<Option<ReqwestClient>>>,
}

#[napi]
impl ObjectStoreClient {
    /// Create a new `ObjectStoreClient` with optional configuration options.
    #[napi(constructor)]
    pub fn new(options: Option<ObjectStoreClientOptions>) -> napi::Result<Self> {
        let opts = options.unwrap_or_default();
        let mut map = HashMap::new();

        map.insert(
            "allow_http".to_string(),
            opts.allow_http.unwrap_or(false).to_string(),
        );
        map.insert(
            "allow_invalid_certificates".to_string(),
            opts.allow_invalid_certificates.unwrap_or(false).to_string(),
        );
        map.insert(
            "disable_system_certificates".to_string(),
            opts.disable_system_certificates
                .unwrap_or(false)
                .to_string(),
        );
        if let Some(ct) = opts.connect_timeout_ms {
            map.insert("connect_timeout".to_string(), format!("{ct}ms"));
        }
        if let Some(t) = opts.timeout_ms {
            map.insert("timeout".to_string(), format!("{t}ms"));
        }
        if let Some(rt) = opts.read_timeout_ms {
            map.insert("read_timeout".to_string(), format!("{rt}ms"));
        }
        if let Some(ua) = &opts.user_agent {
            map.insert("user_agent".to_string(), ua.clone());
        }
        if let Some(pu) = &opts.proxy_url {
            map.insert("proxy_url".to_string(), pu.clone());
        }
        if let Some(pca) = &opts.proxy_ca_certificate {
            map.insert("proxy_ca_certificate".to_string(), pca.clone());
        }
        if let Some(pe) = &opts.proxy_excludes {
            map.insert("proxy_excludes".to_string(), pe.clone());
        }
        if let Some(dct) = &opts.default_content_type {
            map.insert("default_content_type".to_string(), dct.clone());
        }

        let proto = opts
            .transport_protocol
            .unwrap_or_else(|| "http".to_string());
        map.insert("transport_protocol".to_string(), proto);

        if let Some(sk) = &opts.ssh_key_path {
            map.insert("ssh_key_path".to_string(), sk.clone());
        }
        if let Some(su) = &opts.ssh_user {
            map.insert("ssh_user".to_string(), su.clone());
        }
        if let Some(sp) = &opts.ssh_passphrase {
            map.insert("ssh_passphrase".to_string(), sp.clone());
        }

        let reqwest_client = Self::build_reqwest_client(&map)?;

        Ok(ObjectStoreClient {
            config: Arc::new(Mutex::new(map)),
            client: Arc::new(Mutex::new(Some(reqwest_client))),
        })
    }

    fn build_reqwest_client(config: &HashMap<String, String>) -> napi::Result<ReqwestClient> {
        let mut builder = ReqwestClient::builder();

        if let Some(ua) = config.get("user_agent") {
            builder = builder.user_agent(ua);
        } else {
            builder = builder.user_agent("repository/0.1.0");
        }

        let allow_http = config
            .get("allow_http")
            .map(|v| v == "true")
            .unwrap_or(false);
        builder = builder.https_only(!allow_http);

        if config
            .get("allow_invalid_certificates")
            .map(|v| v == "true")
            .unwrap_or(false)
        {
            builder = builder.danger_accept_invalid_certs(true);
        }

        if let Some(ct_str) = config.get("connect_timeout") {
            if let Ok(ms) = ct_str.trim_end_matches("ms").parse::<u64>() {
                builder = builder.connect_timeout(Duration::from_millis(ms));
            }
        } else {
            builder = builder.connect_timeout(Duration::from_secs(5));
        }

        if let Some(t_str) = config.get("timeout") {
            if let Ok(ms) = t_str.trim_end_matches("ms").parse::<u64>() {
                builder = builder.timeout(Duration::from_millis(ms));
            }
        } else {
            builder = builder.timeout(Duration::from_secs(30));
        }

        if let Some(proxy_url) = config.get("proxy_url") {
            if let Ok(proxy) = reqwest::Proxy::all(proxy_url) {
                builder = builder.proxy(proxy);
            }
        }

        builder.build().map_err(|e| err_str("build_client", e.to_string()))
    }

    /// Get a configuration value by key name.
    #[napi]
    pub fn get_config_value(&self, key: String) -> Option<String> {
        self.config.lock().unwrap().get(&key).cloned()
    }

    /// Set or update a configuration key/value pair.
    #[napi]
    pub fn set_config_value(&self, key: String, value: String) -> napi::Result<()> {
        let mut map = self.config.lock().unwrap();
        map.insert(key, value);
        let new_client = Self::build_reqwest_client(&map)?;
        *self.client.lock().unwrap() = Some(new_client);
        Ok(())
    }

    /// Return the active transport protocol (`"http"` or `"ssh"`).
    #[napi(getter)]
    pub fn transport_protocol(&self) -> String {
        self.config
            .lock()
            .unwrap()
            .get("transport_protocol")
            .cloned()
            .unwrap_or_else(|| "http".to_string())
    }

    /// Test connectivity to an HTTP(S) endpoint.
    #[napi]
    pub async fn connect_http(&self, url: String) -> napi::Result<bool> {
        let client_opt = self.client.lock().unwrap().clone();
        let client = client_opt.ok_or_else(|| err_str("connect_http", "HTTP client not initialized"))?;
        match client.head(&url).send().await {
            Ok(resp) => Ok(resp.status().is_success() || resp.status().as_u16() == 401 || resp.status().as_u16() == 403 || resp.status().as_u16() == 404),
            Err(_) => Ok(false),
        }
    }

    /// Test SSH connection by executing a diagnostic query or checking host reachability.
    #[napi]
    pub async fn connect_ssh(
        &self,
        host: String,
        user: Option<String>,
        key_path: Option<String>,
    ) -> napi::Result<bool> {
        let (ssh_user, key) = {
            let map = self.config.lock().unwrap();
            let su = user.or_else(|| map.get("ssh_user").cloned()).unwrap_or_else(|| "git".to_string());
            let k = key_path.or_else(|| map.get("ssh_key_path").cloned());
            (su, k)
        };

        let target = format!("{ssh_user}@{host}");
        let mut cmd = tokio::process::Command::new("ssh");
        cmd.arg("-o").arg("BatchMode=yes");
        cmd.arg("-o").arg("ConnectTimeout=5");
        cmd.arg("-T");

        if let Some(k) = key {
            cmd.arg("-i").arg(k);
        }

        cmd.arg(target);

        match cmd.output().await {
            Ok(out) => Ok(out.status.success()),
            Err(_) => Ok(false),
        }
    }

    /// Execute an HTTP GET request to fetch remote bytes.
    #[napi]
    pub async fn get(&self, url: String) -> napi::Result<Buffer> {
        let client_opt = self.client.lock().unwrap().clone();
        let client = client_opt.ok_or_else(|| err_str("get", "HTTP client not initialized"))?;
        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| err_str("http_get", e.to_string()))?;

        if !resp.status().is_success() {
            return Err(err_str(
                "http_get",
                format!("HTTP error status: {}", resp.status()),
            ));
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| err_str("http_get", e.to_string()))?;
        Ok(Buffer::from(bytes.to_vec()))
    }

    /// Execute an HTTP PUT request to upload bytes to a remote location.
    #[napi]
    pub async fn put(&self, url: String, data: Buffer) -> napi::Result<()> {
        let client_opt = self.client.lock().unwrap().clone();
        let client = client_opt.ok_or_else(|| err_str("put", "HTTP client not initialized"))?;
        let mut req = client.put(&url).body(data.to_vec());

        if let Some(ct) = self.config.lock().unwrap().get("default_content_type") {
            req = req.header("Content-Type", ct);
        }

        let resp = req.send().await.map_err(|e| err_str("http_put", e.to_string()))?;

        if !resp.status().is_success() {
            return Err(err_str(
                "http_put",
                format!("HTTP error status: {}", resp.status()),
            ));
        }

        Ok(())
    }

    /// Execute an HTTP HEAD request to fetch response metadata headers.
    #[napi]
    pub async fn head(&self, url: String) -> napi::Result<HashMap<String, String>> {
        let client_opt = self.client.lock().unwrap().clone();
        let client = client_opt.ok_or_else(|| err_str("head", "HTTP client not initialized"))?;
        let resp = client
            .head(&url)
            .send()
            .await
            .map_err(|e| err_str("http_head", e.to_string()))?;

        let mut headers = HashMap::new();
        for (k, v) in resp.headers().iter() {
            if let Ok(val_str) = v.to_str() {
                headers.insert(k.as_str().to_string(), val_str.to_string());
            }
        }
        Ok(headers)
    }

    /// Execute an HTTP DELETE request.
    #[napi]
    pub async fn delete(&self, url: String) -> napi::Result<()> {
        let client_opt = self.client.lock().unwrap().clone();
        let client = client_opt.ok_or_else(|| err_str("delete", "HTTP client not initialized"))?;
        let resp = client
            .delete(&url)
            .send()
            .await
            .map_err(|e| err_str("http_delete", e.to_string()))?;

        if !resp.status().is_success() {
            return Err(err_str(
                "http_delete",
                format!("HTTP error status: {}", resp.status()),
            ));
        }

        Ok(())
    }
}
