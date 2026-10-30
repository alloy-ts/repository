//! NAPI bindings for network-backed object store services (`ObjectStoreClient`).
//!
//! Supports HTTP/HTTPS (via `reqwest`) and SSH transport connectors for network object stores,
//! and implements the Git [`ObjectStore`](crate::object_store::base::ObjectStore) base trait.

use std::sync::Arc;

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use gix::object::Kind;
use gix::ObjectId;

use crate::error::{err, err_str};
use crate::object::{kind_to_string, string_to_kind};
use crate::object_store::base::ObjectStore;
use crate::object_store::{
    object_kind_from_option, ObjectStoreReader, ObjectStoreWriter, PrefixLookup, WriterTarget,
};

/// Transport type for network object store client (`"http"`, `"https"`, `"ssh"`).
#[napi]
pub enum TransportType {
    Http,
    Https,
    Ssh,
}

/// Options for constructing an [`ObjectStoreClient`].
#[napi(object)]
pub struct ObjectStoreClientOptions {
    /// Object hash kind: `"sha1"` (default) or `"sha256"`.
    pub object_hash: Option<String>,
    /// Request timeout in milliseconds (defaults to 30,000 ms).
    pub timeout_ms: Option<u32>,
    /// Custom User-Agent header for HTTP/HTTPS requests.
    pub user_agent: Option<String>,
    /// SSH private key path or raw key content for SSH transport.
    pub ssh_key: Option<String>,
    /// Allow non-HTTPS HTTP connections.
    pub allow_http: Option<bool>,
}

fn encode_git_object(kind: Kind, data: &[u8]) -> Vec<u8> {
    let header = format!("{} {}\0", kind_to_string(kind), data.len());
    let mut out = Vec::with_capacity(header.len() + data.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(data);
    out
}

fn decode_git_object(raw: &[u8]) -> napi::Result<(Kind, Vec<u8>)> {
    let nul_pos = raw.iter().position(|&b| b == 0).ok_or_else(|| {
        err_str("decode_object", "invalid git object: missing header terminator")
    })?;
    let header_str = std::str::from_utf8(&raw[..nul_pos])
        .map_err(|e| err_str("decode_object", format!("invalid header utf8: {e}")))?;
    let mut parts = header_str.split_whitespace();
    let kind_str = parts
        .next()
        .ok_or_else(|| err_str("decode_object", "missing object kind"))?;
    let _size_str = parts
        .next()
        .ok_or_else(|| err_str("decode_object", "missing object size"))?;
    let kind = string_to_kind(kind_str)?;
    let data = raw[nul_pos + 1..].to_vec();
    Ok((kind, data))
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(future))
    } else {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to create tokio runtime");
        rt.block_on(future)
    }
}

/// Network-backed object store client supporting HTTP/HTTPS and SSH transport connectors.
#[napi]
pub struct ObjectStoreClient {
    client: reqwest::Client,
    base_url: String,
    kind: gix::hash::Kind,
    transport: TransportType,
}

impl ObjectStore for ObjectStoreClient {
    fn kind(&self) -> gix::hash::Kind {
        self.kind
    }

    fn contains_id(&self, id: &str) -> bool {
        let url = format!("{}/objects/{}", self.base_url.trim_end_matches('/'), id);
        block_on(async {
            match self.client.head(&url).send().await {
                Ok(r) => r.status().is_success(),
                Err(_) => false,
            }
        })
    }

    fn lookup_prefix_impl(&self, prefix: &str) -> napi::Result<PrefixLookup> {
        let p = prefix.to_ascii_lowercase();
        let url = format!("{}/objects", self.base_url.trim_end_matches('/'));
        let response = match block_on(async { self.client.get(&url).send().await }) {
            Ok(r) if r.status().is_success() => r,
            _ => return Ok(PrefixLookup { found: false, ambiguous: false, id: None }),
        };

        let keys: Vec<String> = block_on(async { response.json().await }).unwrap_or_default();
        let mut matches = Vec::new();
        for k in keys {
            if k.to_ascii_lowercase().starts_with(&p) {
                matches.push(k);
            }
        }

        if matches.len() == 1 {
            Ok(PrefixLookup {
                found: true,
                ambiguous: false,
                id: Some(matches[0].clone()),
            })
        } else if matches.len() > 1 {
            Ok(PrefixLookup {
                found: false,
                ambiguous: true,
                id: None,
            })
        } else {
            Ok(PrefixLookup {
                found: false,
                ambiguous: false,
                id: None,
            })
        }
    }

    fn find_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectData>> {
        let url = format!("{}/objects/{}", self.base_url.trim_end_matches('/'), id);
        let response = match block_on(async { self.client.get(&url).send().await }) {
            Ok(r) => r,
            Err(e) => return Err(err("find_impl", e)),
        };

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }

        if !response.status().is_success() {
            return Err(err_str("find_impl", format!("HTTP error: {}", response.status())));
        }

        let bytes = block_on(async { response.bytes().await }).map_err(|e| err("find_impl", e))?;
        let (kind, data) = decode_git_object(&bytes)?;
        Ok(Some(crate::object::ObjectData {
            kind: kind_to_string(kind),
            data: Buffer::from(data),
        }))
    }

    fn header_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectHeader>> {
        let url = format!("{}/objects/{}", self.base_url.trim_end_matches('/'), id);
        let response = match block_on(async { self.client.get(&url).send().await }) {
            Ok(r) => r,
            Err(e) => return Err(err("header_impl", e)),
        };

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }

        if !response.status().is_success() {
            return Err(err_str("header_impl", format!("HTTP error: {}", response.status())));
        }

        let bytes = block_on(async { response.bytes().await }).map_err(|e| err("header_impl", e))?;
        let (kind, data) = decode_git_object(&bytes)?;
        Ok(Some(crate::object::ObjectHeader {
            kind: kind_to_string(kind),
            size: data.len() as f64,
            is_packed: false,
            num_deltas: None,
        }))
    }

    fn write_buf_impl(&self, kind: Kind, data: &[u8]) -> napi::Result<ObjectId> {
        let mut h = gix::hash::hasher(self.kind);
        let header = format!("{} {}\0", kind_to_string(kind), data.len());
        h.update(header.as_bytes());
        h.update(data);
        let id = h.try_finalize().map_err(|e| err("write_object", e))?;

        let raw = encode_git_object(kind, data);
        let url = format!("{}/objects/{}", self.base_url.trim_end_matches('/'), id);
        let response = block_on(async { self.client.put(&url).body(raw).send().await })
            .map_err(|e| err("write_object", e))?;

        if !response.status().is_success() {
            return Err(err_str("write_object", format!("HTTP error: {}", response.status())));
        }

        Ok(id)
    }

    fn write_buf_with_id_impl(
        &self,
        kind: Kind,
        data: &[u8],
        id: ObjectId,
    ) -> napi::Result<ObjectId> {
        let raw = encode_git_object(kind, data);
        let url = format!("{}/objects/{}", self.base_url.trim_end_matches('/'), id);
        let response = block_on(async { self.client.put(&url).body(raw).send().await })
            .map_err(|e| err("write_object_with_id", e))?;

        if !response.status().is_success() {
            return Err(err_str("write_object_with_id", format!("HTTP error: {}", response.status())));
        }

        Ok(id)
    }

    fn object_path_impl(&self, _id: &str) -> napi::Result<String> {
        Err(err_str(
            "object_path",
            "network object store client does not expose local object file paths",
        ))
    }

    fn store_path(&self) -> String {
        self.base_url.clone()
    }

    fn write_target(&self) -> WriterTarget {
        let client = self.client.clone();
        let base_url = self.base_url.clone();
        let hash_kind = self.kind;
        WriterTarget::Custom(Arc::new(move |obj_kind, data| {
            let mut h = gix::hash::hasher(hash_kind);
            let header = format!("{} {}\0", kind_to_string(obj_kind), data.len());
            h.update(header.as_bytes());
            h.update(data);
            let id = h.try_finalize().map_err(|e| err("write_object", e))?;

            let raw = encode_git_object(obj_kind, data);
            let url = format!("{}/objects/{}", base_url.trim_end_matches('/'), id);
            let response = block_on(async { client.put(&url).body(raw).send().await })
                .map_err(|e| err("write_object", e))?;

            if !response.status().is_success() {
                return Err(err_str("write_object", format!("HTTP error: {}", response.status())));
            }

            Ok(id)
        }))
    }

    fn read_data_impl(&self, id: &str) -> napi::Result<ObjectStoreReader> {
        let url = format!("{}/objects/{}", self.base_url.trim_end_matches('/'), id);
        let response = block_on(async { self.client.get(&url).send().await })
            .map_err(|e| err("reader", e))?;

        if !response.status().is_success() {
            return Err(err_str("reader", format!("object {id} not found")));
        }

        let bytes = block_on(async { response.bytes().await }).map_err(|e| err("reader", e))?;
        let (kind, data) = decode_git_object(&bytes)?;
        Ok(ObjectStoreReader {
            data,
            kind: kind_to_string(kind),
            pos: 0,
            max_chunk_size: None,
        })
    }

    fn insert_objects(
        &self,
        objects: Vec<(ObjectId, (Kind, Vec<u8>))>,
    ) -> napi::Result<()> {
        for (id, (kind, data)) in objects {
            self.write_buf_with_id_impl(kind, &data, id)?;
        }
        Ok(())
    }
}

#[napi]
impl ObjectStoreClient {
    /// Create a new `ObjectStoreClient` connecting to `base_url` over HTTP, HTTPS, or SSH.
    #[napi(constructor)]
    pub fn new(
        base_url: String,
        options: Option<napi::bindgen_prelude::Either<String, ObjectStoreClientOptions>>,
    ) -> napi::Result<Self> {
        let (hash_str, client_opts) = parse_hash_or_options_client(options);
        let kind = object_kind_from_option(hash_str.as_deref())?;
        let opts = client_opts.unwrap_or(ObjectStoreClientOptions {
            object_hash: None,
            timeout_ms: None,
            user_agent: None,
            ssh_key: None,
            allow_http: None,
        });

        let transport = if base_url.starts_with("ssh://") || base_url.starts_with("git@") {
            TransportType::Ssh
        } else if base_url.starts_with("https://") {
            TransportType::Https
        } else {
            TransportType::Http
        };

        let mut builder = reqwest::Client::builder();
        if let Some(timeout) = opts.timeout_ms {
            builder = builder.timeout(std::time::Duration::from_millis(timeout as u64));
        } else {
            builder = builder.timeout(std::time::Duration::from_secs(30));
        }

        if let Some(ua) = opts.user_agent {
            builder = builder.user_agent(ua);
        } else {
            builder = builder.user_agent("repository-napi-object-store-client/0.1");
        }

        let client = builder.build().map_err(|e| err("ObjectStoreClient::new", e))?;

        Ok(Self {
            client,
            base_url,
            kind,
            transport,
        })
    }

    /// Return the transport type used by this client (`Http`, `Https`, or `Ssh`).
    #[napi(getter)]
    pub fn transport_type(&self) -> TransportType {
        match self.transport {
            TransportType::Http => TransportType::Http,
            TransportType::Https => TransportType::Https,
            TransportType::Ssh => TransportType::Ssh,
        }
    }

    /// Whether an object with the given id exists.
    #[napi]
    pub fn contains(&self, id: String) -> bool {
        ObjectStore::contains(self, id)
    }

    /// Resolve a short object id prefix.
    #[napi]
    pub fn lookup_prefix(&self, prefix: String) -> napi::Result<PrefixLookup> {
        ObjectStore::lookup_prefix(self, prefix)
    }

    /// Read an object's body and kind.
    #[napi]
    pub fn try_find(&self, id: String) -> napi::Result<Option<crate::object::ObjectData>> {
        ObjectStore::try_find(self, id)
    }

    /// Read an object's header (kind + size).
    #[napi]
    pub fn try_header(&self, id: String) -> napi::Result<Option<crate::object::ObjectHeader>> {
        ObjectStore::try_header(self, id)
    }

    /// Write a raw object of the given `kind` and return its id.
    #[napi]
    pub fn write_object(&self, kind: String, data: Buffer) -> napi::Result<String> {
        ObjectStore::write_object(self, kind, data)
    }

    /// Write a raw object under the exact `id`.
    #[napi]
    pub fn write_object_with_id(
        &self,
        kind: String,
        data: Buffer,
        id: String,
    ) -> napi::Result<String> {
        ObjectStore::write_object_with_id(self, kind, data, id)
    }

    /// Return the base URL path.
    #[napi(getter)]
    pub fn path(&self) -> String {
        ObjectStore::path(self)
    }

    /// Network client stores do not expose local object file paths; this errors.
    #[napi]
    pub fn object_path(&self, id: String) -> napi::Result<String> {
        ObjectStore::object_path(self, id)
    }

    /// Return the object hash kind (`"sha1"` or `"sha256"`).
    #[napi(getter)]
    pub fn object_hash(&self) -> String {
        ObjectStore::object_hash(self)
    }

    /// Return the object format lower-cased (`"sha1"` or `"sha256"`).
    #[napi(getter)]
    pub fn object_format(&self) -> String {
        ObjectStore::object_format(self)
    }

    /// Open a streaming reader for `id`.
    #[napi]
    pub fn reader(&self, id: String) -> napi::Result<ObjectStoreReader> {
        ObjectStore::reader(self, id)
    }

    /// Open a streaming writer for `kind` and `size`.
    #[napi]
    pub fn writer(&self, kind: String, size: f64) -> napi::Result<ObjectStoreWriter> {
        ObjectStore::writer(self, kind, size)
    }

    /// Ingest a packfile `Buffer` into this store.
    #[napi]
    pub fn import_stream(&self, pack: Buffer) -> napi::Result<()> {
        ObjectStore::import_stream(self, pack)
    }
}

fn parse_hash_or_options_client(
    opt: Option<napi::bindgen_prelude::Either<String, ObjectStoreClientOptions>>,
) -> (Option<String>, Option<ObjectStoreClientOptions>) {
    match opt {
        Some(napi::bindgen_prelude::Either::A(hash)) => (Some(hash), None),
        Some(napi::bindgen_prelude::Either::B(opts)) => (opts.object_hash.clone(), Some(opts)),
        None => (None, None),
    }
}
