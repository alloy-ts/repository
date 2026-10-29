//! An [`ObjectStore`](crate::object_store::base::ObjectStore) variant backed by the Apache
//! `object_store` crate.
//!
//! This is the "universal" backend: a single napi class that can sit on top of any of `object_store`'s
//! implementations — [`LocalFileSystem`], [`InMemory`], or cloud stores (S3/GCS/Azure) — by holding an
//! `Arc<dyn object_store::ObjectStore>`. It reuses the shared [`base::ObjectStore`] trait, so it exposes
//! exactly the same napi surface as [`crate::object_store::LooseObjectStore`] and
//! [`crate::object_store::MemoryObjectStore`].
//!
//! Objects are stored as raw git objects (`"<kind> <size>\0<payload>"`) under a two-character fanout
//! path (`ab/cdef…`), keyed by their content hash — i.e. the same layout git uses, minus the zlib
//! compression. This keeps the store a drop-in alternative to the loose store for our purposes while
//! remaining a plain blob store underneath.

use std::path::{Path as StdPath, PathBuf};
use std::sync::{Arc, OnceLock};

use futures::StreamExt;

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use gix::ObjectId;
use gix::object::Kind;

use crate::error::{err, err_str};
use crate::object::kind_to_string;
use crate::object_store::base::ObjectStore;
use crate::object_store::{
    ObjectStoreReader, ObjectStoreWriter, PrefixLookup, WriterTarget, object_kind_from_option,
};
use object_store::{ObjectStore as _, ObjectStoreExt};

/// A dedicated tokio runtime used to bridge the `object_store` crate's async API to the
/// synchronous napi methods. The `object_store` backends perform their I/O via blocking `std::fs`
/// calls inside `maybe_spawn_blocking`, so only the scheduler/blocking-pool is required (no IO
/// driver).
fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .build()
            .expect("arrow object store: failed to build tokio runtime")
    })
}

/// Build a fanout key (`ab/cdef…`) from a hex object id. `object_store`'s `Path` uses `/` as a
/// directory separator, so the first two hex chars become a directory and the rest the filename.
fn fanout(id: &str) -> String {
    format!("{}/{}", &id[0..2], &id[2..])
}

/// Assemble the raw git object bytes (`"<kind> <size>\0<payload>"`) we persist.
fn build_raw(kind: Kind, data: &[u8]) -> Vec<u8> {
    let header = format!("{} {}\0", kind_to_string(kind), data.len());
    let mut v = header.into_bytes();
    v.extend_from_slice(data);
    v
}

/// Compute an object's content hash from its kind + payload.
fn hash_id(kind: Kind, data: &[u8], hk: gix::hash::Kind) -> napi::Result<ObjectId> {
    let mut h = gix::hash::hasher(hk);
    let header = format!("{} {}\0", kind_to_string(kind), data.len());
    h.update(header.as_bytes());
    h.update(data);
    h.try_finalize().map_err(|e| err("arrow", e))
}

/// Split the raw git object bytes into `(kind, payload)` by parsing the leading header.
fn parse_loose(bytes: &[u8]) -> napi::Result<(Kind, Vec<u8>)> {
    let nul = bytes
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| err_str("arrow", "object bytes missing header NUL terminator"))?;
    let header = &bytes[..nul];
    let sp = header
        .iter()
        .position(|&b| b == b' ')
        .ok_or_else(|| err_str("arrow", "object header missing space separator"))?;
    let kind = match &header[..sp] {
        b"blob" => Kind::Blob,
        b"tree" => Kind::Tree,
        b"commit" => Kind::Commit,
        b"tag" => Kind::Tag,
        _ => return Err(err_str("arrow", "unknown object kind in header")),
    };
    Ok((kind, bytes[nul + 1..].to_vec()))
}

/// The storage backend: an `Arc<dyn object_store::ObjectStore>` plus the metadata we need to bridge
/// back to git (hash kind, a human-readable root, and — for local filesystems — the on-disk prefix so
/// we can resolve `object_path`).
pub(crate) struct ArrowStore {
    store: Arc<dyn object_store::ObjectStore>,
    kind: gix::hash::Kind,
    root: String,
    fs_prefix: Option<PathBuf>,
}

impl ArrowStore {
    /// Persist the raw git object bytes under `id`'s fanout key.
    fn put(&self, id: &str, raw: Vec<u8>) -> napi::Result<()> {
        let loc = object_store::path::Path::parse(fanout(id)).map_err(|e| err("arrow", e))?;
        let payload = object_store::PutPayload::from(raw);
        runtime()
            .block_on(self.store.put(&loc, payload))
            .map_err(|e| err("arrow_put", e))?;
        Ok(())
    }

    /// Fetch the raw git object bytes for `id`, or `None` if absent.
    fn get(&self, id: &str) -> napi::Result<Option<Vec<u8>>> {
        let loc = object_store::path::Path::parse(fanout(id)).map_err(|e| err("arrow", e))?;
        match runtime().block_on(self.store.get(&loc)) {
            Ok(res) => {
                let bytes = runtime()
                    .block_on(res.bytes())
                    .map_err(|e| err("arrow_get", e))?;
                Ok(Some(bytes.to_vec()))
            }
            Err(e) if matches!(e, object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(err("arrow_get", e)),
        }
    }

    /// Whether `id` exists in the store.
    fn head(&self, id: &str) -> bool {
        let Ok(loc) = object_store::path::Path::parse(fanout(id)) else {
            return false;
        };
        runtime().block_on(self.store.head(&loc)).is_ok()
    }

    /// Enumerate every object id currently in the store.
    ///
    /// Uses the recursive `list` stream (not `list_with_delimiter`, whose delimiter semantics would
    /// collapse our two-segment fanout keys `ab/cdef…` into a "directory" `ab` plus a child, hiding
    /// the objects from the returned `objects` list).
    fn list_ids(&self) -> napi::Result<Vec<String>> {
        let mut stream = self.store.list(None);
        let mut out = Vec::new();
        loop {
            match runtime().block_on(StreamExt::next(&mut stream)) {
                Some(Ok(meta)) => out.push(meta.location.to_string().replace('/', "")),
                Some(Err(e)) => return Err(err("arrow_list", e)),
                None => break,
            }
        }
        Ok(out)
    }

    /// Write a fresh object, hashing its content to derive the id.
    pub(crate) fn write_object(&self, kind: Kind, data: &[u8]) -> napi::Result<ObjectId> {
        let id = hash_id(kind, data, self.kind)?;
        self.put(&id.to_string(), build_raw(kind, data))?;
        Ok(id)
    }

    /// Write an object under a caller-supplied id (hash not recomputed).
    fn write_with_id(&self, id: ObjectId, kind: Kind, data: &[u8]) -> napi::Result<()> {
        self.put(&id.to_string(), build_raw(kind, data))
    }
}

/// Build the backing `Arc<dyn object_store::ObjectStore>` and its metadata from a URI/root string.
///
/// `""`, `"memory"`, or `"memory://"` select the in-memory backend; anything else is treated as a
/// filesystem path (an optional `file://` scheme is stripped) and mounted via `LocalFileSystem`.
fn build(uri: &str) -> napi::Result<(Arc<dyn object_store::ObjectStore>, String, Option<PathBuf>)> {
    let trimmed = uri.trim();
    if trimmed.is_empty() || trimmed == "memory" || trimmed.starts_with("memory://") {
        let mem = Arc::new(object_store::memory::InMemory::new());
        let store: Arc<dyn object_store::ObjectStore> = mem;
        return Ok((store, "memory".to_string(), None));
    }

    let path = if let Some(p) = trimmed.strip_prefix("file://") {
        p.to_string()
    } else {
        trimmed.to_string()
    };
    let p = StdPath::new(&path);
    std::fs::create_dir_all(p).map_err(|e| err("arrow_new", e))?;
    let canon = std::fs::canonicalize(p).map_err(|e| err("arrow_new", e))?;
    let fs = object_store::local::LocalFileSystem::new_with_prefix(&canon)
        .map_err(|e| err("arrow_new", e))?;
    let store: Arc<dyn object_store::ObjectStore> = Arc::new(fs);
    let root = canon.to_string_lossy().to_string();
    Ok((store, root, Some(canon)))
}

/// A git object store backed by the Apache `object_store` crate — see the module docs for the
/// rationale. It reuses the shared [`base::ObjectStore`] trait, so its napi surface matches
/// [`crate::object_store::LooseObjectStore`] and [`crate::object_store::MemoryObjectStore`].
#[napi]
pub struct ArrowObjectStore {
    inner: Arc<ArrowStore>,
}

impl ObjectStore for ArrowObjectStore {
    fn kind(&self) -> gix::hash::Kind {
        self.inner.kind
    }

    fn contains_id(&self, id: &str) -> bool {
        self.inner.head(id)
    }

    fn lookup_prefix_impl(&self, prefix: &str) -> napi::Result<PrefixLookup> {
        let p = prefix.to_ascii_lowercase();
        let mut found: Option<String> = None;
        let mut count = 0usize;
        for id in self.inner.list_ids()? {
            if id.starts_with(&p) {
                count += 1;
                found = Some(id);
            }
        }
        Ok(match found {
            Some(id) if count == 1 => PrefixLookup {
                found: true,
                ambiguous: false,
                id: Some(id),
            },
            Some(_) => PrefixLookup {
                found: false,
                ambiguous: true,
                id: None,
            },
            None => PrefixLookup {
                found: false,
                ambiguous: false,
                id: None,
            },
        })
    }

    fn find_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectData>> {
        match self.inner.get(id)? {
            None => Ok(None),
            Some(bytes) => {
                let (kind, data) = parse_loose(&bytes)?;
                Ok(Some(crate::object::ObjectData {
                    kind: kind_to_string(kind),
                    data: Buffer::from(data),
                }))
            }
        }
    }

    fn header_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectHeader>> {
        match self.inner.get(id)? {
            None => Ok(None),
            Some(bytes) => {
                let (kind, data) = parse_loose(&bytes)?;
                Ok(Some(crate::object::ObjectHeader {
                    kind: kind_to_string(kind),
                    size: data.len() as f64,
                    is_packed: false,
                    num_deltas: None,
                }))
            }
        }
    }

    fn write_buf_impl(&self, kind: Kind, data: &[u8]) -> napi::Result<ObjectId> {
        self.inner.write_object(kind, data)
    }

    fn write_buf_with_id_impl(
        &self,
        kind: Kind,
        data: &[u8],
        id: ObjectId,
    ) -> napi::Result<ObjectId> {
        self.inner.write_with_id(id, kind, data)?;
        Ok(id)
    }

    fn object_path_impl(&self, id: &str) -> napi::Result<String> {
        match &self.inner.fs_prefix {
            Some(prefix) => {
                let mut p = prefix.clone();
                for part in fanout(id).split('/') {
                    p = p.join(part);
                }
                Ok(p.to_string_lossy().to_string())
            }
            None => Err(err_str(
                "object_path",
                "arrow store is not a local filesystem store",
            )),
        }
    }

    fn store_path(&self) -> String {
        self.inner.root.clone()
    }

    fn write_target(&self) -> WriterTarget {
        WriterTarget::Arrow(self.inner.clone())
    }

    fn read_data_impl(&self, id: &str) -> napi::Result<ObjectStoreReader> {
        match self.inner.get(id)? {
            None => Err(err_str("reader", format!("object {id} not found"))),
            Some(bytes) => {
                let (kind, data) = parse_loose(&bytes)?;
                Ok(ObjectStoreReader {
                    data,
                    kind: kind_to_string(kind),
                    pos: 0,
                })
            }
        }
    }

    fn insert_objects(&self, objects: Vec<(ObjectId, (Kind, Vec<u8>))>) -> napi::Result<()> {
        for (id, (kind, data)) in objects {
            self.inner.write_with_id(id, kind, &data)?;
        }
        Ok(())
    }
}

#[napi]
impl ArrowObjectStore {
    /// Create a new `object_store`-backed object store.
    ///
    /// - `uri` is `"memory"` (or `""`) for an in-memory store, or a filesystem path (optionally
    ///   prefixed with `file://`) for a local-disk store (backed by `LocalFileSystem`).
    /// - `object_hash` is `"sha1"` (default) or `"sha256"`.
    #[napi(constructor)]
    pub fn new(uri: String, object_hash: Option<String>) -> napi::Result<Self> {
        let kind = object_kind_from_option(object_hash.as_deref())?;
        let (store, root, fs_prefix) = build(&uri)?;
        Ok(Self {
            inner: Arc::new(ArrowStore {
                store,
                kind,
                root,
                fs_prefix,
            }),
        })
    }

    /// Whether an object with the given id exists in the store.
    #[napi]
    pub fn contains(&self, id: String) -> bool {
        ObjectStore::contains(self, id)
    }

    /// Resolve a short object id `prefix` (hex) to a unique object.
    #[napi]
    pub fn lookup_prefix(&self, prefix: String) -> napi::Result<PrefixLookup> {
        ObjectStore::lookup_prefix(self, prefix)
    }

    /// Read an object's raw, decompressed body and kind, or `null` if missing.
    #[napi]
    pub fn try_find(&self, id: String) -> napi::Result<Option<crate::object::ObjectData>> {
        ObjectStore::try_find(self, id)
    }

    /// Read an object's header (kind + size) without decoding its full content, or `null`.
    #[napi]
    pub fn try_header(&self, id: String) -> napi::Result<Option<crate::object::ObjectHeader>> {
        ObjectStore::try_header(self, id)
    }

    /// Write a raw object of the given `kind` (`commit`, `tree`, `blob`, `tag`) and return its id.
    #[napi]
    pub fn write_object(&self, kind: String, data: Buffer) -> napi::Result<String> {
        ObjectStore::write_object(self, kind, data)
    }

    /// Write a raw object of the given `kind` under the exact `id`, without recomputing its hash.
    #[napi]
    pub fn write_object_with_id(
        &self,
        kind: String,
        data: Buffer,
        id: String,
    ) -> napi::Result<String> {
        ObjectStore::write_object_with_id(self, kind, data, id)
    }

    /// The on-disk path of the object file for `id` (errors for in-memory stores).
    #[napi]
    pub fn object_path(&self, id: String) -> napi::Result<String> {
        ObjectStore::object_path(self, id)
    }

    /// The store's root path (or `"memory"` for the in-memory store).
    #[napi(getter)]
    pub fn path(&self) -> String {
        ObjectStore::path(self)
    }

    /// Return the hash kind this store uses (`"sha1"` or `"sha256"`).
    #[napi(getter)]
    pub fn object_hash(&self) -> String {
        ObjectStore::object_hash(self)
    }

    /// The object format this store uses, lower-cased (`"sha1"` or `"sha256"`), for parity with
    /// [`crate::repository::Repository::objectFormat`].
    #[napi(getter)]
    pub fn object_format(&self) -> String {
        ObjectStore::object_format(self)
    }

    /// Open a streaming reader for the object identified by `id`.
    #[napi]
    pub fn reader(&self, id: String) -> napi::Result<ObjectStoreReader> {
        ObjectStore::reader(self, id)
    }

    /// Open a streaming writer for a new object of the given `kind` and total `size` in bytes.
    #[napi]
    pub fn writer(&self, kind: String, size: f64) -> napi::Result<ObjectStoreWriter> {
        ObjectStore::writer(self, kind, size)
    }

    /// Ingest a packfile `Buffer` into the store, adding all of its objects.
    #[napi]
    pub fn import_stream(&self, pack: Buffer) -> napi::Result<()> {
        ObjectStore::import_stream(self, pack)
    }

    /// Return the hex ids of every object currently in the store.
    #[napi]
    pub fn entries(&self) -> napi::Result<Vec<String>> {
        self.inner.list_ids()
    }

    /// Return the number of objects currently in the store.
    #[napi]
    pub fn size(&self) -> napi::Result<f64> {
        Ok(self.inner.list_ids()?.len() as f64)
    }
}
