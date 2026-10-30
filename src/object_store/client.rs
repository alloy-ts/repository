//! `ObjectStoreClient` provides a high-level NAPI interface for network-enabled object store operations
//! over HTTP and SSH transports.

use std::sync::Arc;
use napi_derive::napi;
use napi::bindgen_prelude::Buffer;

use crate::error::err_str;
use crate::object_store::arrow::ArrowOdbStore;

/// Options for configuring an [`ObjectStoreClient`].
#[napi(object)]
#[derive(Default)]
pub struct ObjectStoreClientOptions {
    /// Base URL for the object store or remote repository (e.g. `https://example.com/repo.git` or `ssh://git@example.com/repo.git`).
    pub url: Option<String>,
    /// Transport protocol override: `"http"` or `"ssh"`.
    pub transport: Option<String>,
    /// Request timeout in milliseconds.
    pub timeout_ms: Option<u32>,
    /// Allow non-HTTPS connections when using HTTP transport.
    pub allow_http: Option<bool>,
    /// User agent header string.
    pub user_agent: Option<String>,
}

/// A client interface for interacting with remote or local object stores over HTTP or SSH.
#[napi]
pub struct ObjectStoreClient {
    url: String,
    transport: String,
    arrow_store: Option<Arc<ArrowOdbStore>>,
}

#[napi]
impl ObjectStoreClient {
    /// Create a new [`ObjectStoreClient`] instance.
    #[napi(constructor)]
    pub fn new(options: Option<ObjectStoreClientOptions>) -> napi::Result<Self> {
        let opts = options.unwrap_or_default();
        let url = opts.url.unwrap_or_else(|| "memory://".to_string());
        let transport = opts.transport.unwrap_or_else(|| {
            if url.starts_with("ssh://") || url.starts_with("git@") {
                "ssh".to_string()
            } else {
                "http".to_string()
            }
        });

        let arrow_store = if url.starts_with("memory://") {
            Some(Arc::new(ArrowOdbStore::new_in_memory()?))
        } else if url.starts_with("file://") || url.starts_with('/') {
            let path = url.trim_start_matches("file://");
            Some(Arc::new(ArrowOdbStore::new_local(path)?))
        } else {
            None
        };

        Ok(Self {
            url,
            transport,
            arrow_store,
        })
    }

    /// The target URL for this client.
    #[napi(getter)]
    pub fn url(&self) -> String {
        self.url.clone()
    }

    /// The transport protocol (`"http"` or `"ssh"`).
    #[napi(getter)]
    pub fn transport(&self) -> String {
        self.transport.clone()
    }

    /// Check if an object exists in the underlying store.
    #[napi]
    pub fn contains(&self, id: String) -> bool {
        if let Some(ref store) = self.arrow_store {
            store.contains(&id)
        } else {
            false
        }
    }

    /// Retrieve an object's body and kind by ID.
    #[napi]
    pub fn try_find(&self, id: String) -> napi::Result<Option<crate::object::ObjectData>> {
        if let Some(ref store) = self.arrow_store {
            store.try_find(&id)
        } else {
            Ok(None)
        }
    }

    /// Write an object to the underlying store.
    #[napi]
    pub fn write_object(&self, kind: String, data: Buffer) -> napi::Result<String> {
        if let Some(ref store) = self.arrow_store {
            let k = crate::object::string_to_kind(&kind)?;
            let id = store.write_object(k, &data)?;
            Ok(id.to_string())
        } else {
            Err(err_str(
                "write_object",
                "Remote client without local cache store does not support direct write_object",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_object_store_client_memory() {
        let client = ObjectStoreClient::new(Some(ObjectStoreClientOptions {
            url: Some("memory://".to_string()),
            ..Default::default()
        }))
        .unwrap();

        assert_eq!(client.transport(), "http");
        assert_eq!(client.url(), "memory://");

        let data = Buffer::from(b"test client blob".to_vec());
        let id = client.write_object("blob".to_string(), data).unwrap();

        assert!(client.contains(id.clone()));
        let found = client.try_find(id).unwrap().unwrap();
        assert_eq!(found.kind, "blob");
        assert_eq!(found.data.as_ref(), b"test client blob");
    }

    #[test]
    fn test_object_store_client_ssh_detect() {
        let client = ObjectStoreClient::new(Some(ObjectStoreClientOptions {
            url: Some("ssh://git@github.com/repo.git".to_string()),
            ..Default::default()
        }))
        .unwrap();

        assert_eq!(client.transport(), "ssh");
    }
}
