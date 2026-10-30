//! An [`ObjectStore`](crate::object_store::base::ObjectStore) implementation backed by Apache Arrow's
//! [`object_store`](object_store) crate, and vice-versa.
//!
//! [`ArrowObjectStore`] adapts any Arrow object store backend (local filesystem, S3, Azure, GCP,
//! in-memory, etc.) into a Git Object Database store.
//!
//! [`GitAsArrowObjectStore`] adapts any Git [`ObjectStore`](crate::object_store::base::ObjectStore)
//! into Apache Arrow's `object_store::ObjectStore` trait.

use std::sync::Arc;

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use object_store::path::Path as ArrowPath;
use object_store::{ObjectStore as ArrowStoreTrait, ObjectStoreExt};

use gix::object::Kind;
use gix::ObjectId;

use crate::error::{err, err_str};
use crate::object::{kind_to_string, string_to_kind};
use crate::object_store::base::ObjectStore;
use crate::object_store::{
    object_kind_from_option, parse_hash_or_options, ObjectStoreOptions, ObjectStoreReader,
    ObjectStoreWriter, PrefixLookup, WriterTarget,
};

/// Format a Git object id into an Arrow store path location: `objects/<id>`.
fn object_location(id: &str) -> ArrowPath {
    ArrowPath::from(format!("objects/{id}"))
}

/// Helper to execute async `object_store` futures synchronously within NAPI methods.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(future))
    } else {
        futures::executor::block_on(future)
    }
}

/// Parse a raw git object payload (`"<kind> <len>\0<data>"`) or separate header/data into `(Kind, Vec<u8>)`.
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

/// An object database store backed by Apache Arrow's `object_store` crate.
///
/// Wraps any [`object_store::ObjectStore`] implementation (such as local filesystem, in-memory, S3,
/// Azure, GCP, etc.) and presents it through the Git [`ObjectStore`] base trait.
#[napi]
pub struct ArrowObjectStore {
    inner: Arc<dyn ArrowStoreTrait>,
    kind: gix::hash::Kind,
    root_path: String,
}

impl ArrowObjectStore {
    /// Create a new `ArrowObjectStore` wrapping an arbitrary Arrow object store backend.
    pub fn new_with_store(
        store: Arc<dyn ArrowStoreTrait>,
        object_hash: gix::hash::Kind,
        root_path: String,
    ) -> Self {
        Self {
            inner: store,
            kind: object_hash,
            root_path,
        }
    }

    /// Access the underlying Arrow object store trait object.
    pub fn inner(&self) -> &Arc<dyn ArrowStoreTrait> {
        &self.inner
    }
}

impl ObjectStore for ArrowObjectStore {
    fn kind(&self) -> gix::hash::Kind {
        self.kind
    }

    fn contains_id(&self, id: &str) -> bool {
        let loc = object_location(id);
        block_on(async { self.inner.head(&loc).await.is_ok() })
    }

    fn lookup_prefix_impl(&self, prefix: &str) -> napi::Result<PrefixLookup> {
        let p = prefix.to_ascii_lowercase();
        let prefix_loc = ArrowPath::from("objects");
        let results = block_on(async {
            use futures::StreamExt;
            let mut stream = self.inner.list(Some(&prefix_loc));
            let mut matches = Vec::new();
            while let Some(res) = stream.next().await {
                if let Ok(meta) = res {
                    let filename = meta.location.filename().unwrap_or_default();
                    if filename.to_ascii_lowercase().starts_with(&p) {
                        matches.push(filename.to_string());
                    }
                }
            }
            matches
        });

        if results.len() == 1 {
            Ok(PrefixLookup {
                found: true,
                ambiguous: false,
                id: Some(results[0].clone()),
            })
        } else if results.len() > 1 {
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
        let loc = object_location(id);
        let bytes = match block_on(async {
            let res = self.inner.get(&loc).await?;
            res.bytes().await
        }) {
            Ok(b) => b,
            Err(object_store::Error::NotFound { .. }) => return Ok(None),
            Err(e) => return Err(err("find_impl", e)),
        };

        let (kind, data) = decode_git_object(&bytes)?;
        Ok(Some(crate::object::ObjectData {
            kind: kind_to_string(kind),
            data: Buffer::from(data),
        }))
    }

    fn header_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectHeader>> {
        let loc = object_location(id);
        let bytes = match block_on(async {
            let res = self.inner.get(&loc).await?;
            res.bytes().await
        }) {
            Ok(b) => b,
            Err(object_store::Error::NotFound { .. }) => return Ok(None),
            Err(e) => return Err(err("header_impl", e)),
        };

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
        let loc = object_location(&id.to_string());
        let payload = object_store::PutPayload::from(raw);

        block_on(async { self.inner.put(&loc, payload).await })
            .map_err(|e| err("write_object", e))?;

        Ok(id)
    }

    fn write_buf_with_id_impl(
        &self,
        kind: Kind,
        data: &[u8],
        id: ObjectId,
    ) -> napi::Result<ObjectId> {
        let raw = encode_git_object(kind, data);
        let loc = object_location(&id.to_string());
        let payload = object_store::PutPayload::from(raw);

        block_on(async { self.inner.put(&loc, payload).await })
            .map_err(|e| err("write_object_with_id", e))?;

        Ok(id)
    }

    fn object_path_impl(&self, _id: &str) -> napi::Result<String> {
        Err(err_str(
            "object_path",
            "arrow object store backends do not expose direct local file paths",
        ))
    }

    fn store_path(&self) -> String {
        self.root_path.clone()
    }

    fn write_target(&self) -> WriterTarget {
        let inner = self.inner.clone();
        let kind = self.kind;
        WriterTarget::Custom(Arc::new(move |obj_kind, data| {
            let mut h = gix::hash::hasher(kind);
            let header = format!("{} {}\0", kind_to_string(obj_kind), data.len());
            h.update(header.as_bytes());
            h.update(data);
            let id = h.try_finalize().map_err(|e| err("write_object", e))?;

            let raw = encode_git_object(obj_kind, data);
            let loc = object_location(&id.to_string());
            let payload = object_store::PutPayload::from(raw);

            block_on(async { inner.put(&loc, payload).await })
                .map_err(|e| err("write_object", e))?;

            Ok(id)
        }))
    }

    fn read_data_impl(&self, id: &str) -> napi::Result<ObjectStoreReader> {
        let loc = object_location(id);
        let bytes = match block_on(async {
            let res = self.inner.get(&loc).await?;
            res.bytes().await
        }) {
            Ok(b) => b,
            Err(e) => return Err(err("reader", e)),
        };

        let (kind, data) = decode_git_object(&bytes)?;
        Ok(ObjectStoreReader {
            data,
            kind: kind_to_string(kind),
            pos: 0,
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
impl ArrowObjectStore {
    /// Create a local-filesystem backed Arrow object store at `directory`.
    #[napi(constructor)]
    pub fn new(
        directory: String,
        object_hash: Option<napi::bindgen_prelude::Either<String, ObjectStoreOptions>>,
    ) -> napi::Result<Self> {
        let (hash_str, _) = parse_hash_or_options(object_hash);
        let kind = object_kind_from_option(hash_str.as_deref())?;
        std::fs::create_dir_all(&directory).map_err(|e| err("ArrowObjectStore::new", e))?;
        let local_store = object_store::local::LocalFileSystem::new_with_prefix(&directory)
            .map_err(|e| err("ArrowObjectStore::new", e))?;
        Ok(Self {
            inner: Arc::new(local_store),
            kind,
            root_path: directory,
        })
    }

    /// Create an in-memory backed Arrow object store.
    #[napi(factory)]
    pub fn create_in_memory(
        object_hash: Option<napi::bindgen_prelude::Either<String, ObjectStoreOptions>>,
    ) -> napi::Result<Self> {
        let (hash_str, _) = parse_hash_or_options(object_hash);
        let kind = object_kind_from_option(hash_str.as_deref())?;
        let mem_store = object_store::memory::InMemory::new();
        Ok(Self {
            inner: Arc::new(mem_store),
            kind,
            root_path: "arrow::memory".to_string(),
        })
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

    /// Return the store path.
    #[napi(getter)]
    pub fn path(&self) -> String {
        ObjectStore::path(self)
    }

    /// Arrow stores do not expose local object file paths; this errors.
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
