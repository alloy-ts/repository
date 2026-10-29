//! Shared base for the lower-level object stores.
//!
//! [`LooseObjectStore`] and [`MemoryObjectStore`] both implement [`ObjectStore`], which factors out
//! their common napi surface. Each variant supplies only the storage-specific primitives via the
//! *required* trait methods; the *default* methods implement the napi-facing glue (exists / lookup /
//! read / write / stream / `import_stream`) once. The default `import_stream` decodes a packfile
//! via [`decode_pack`] and hands the objects to `insert_objects`.

use std::path::{Path as StdPath, PathBuf};
use std::sync::{Arc, OnceLock};

use futures::StreamExt;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use gix::ObjectId;
use gix::object::Kind;
use gix::zlib::{Decompress, Inflate};

use crate::error::{err, err_str, parse_oid};
use crate::object::{kind_to_string, string_to_kind};
use crate::object_store::hash_kind_format;
use crate::object_store::{
    ObjectStoreReader, ObjectStoreWriter, PrefixLookup, WriterTarget, client::ObjectStoreClient,
    object_kind_from_option,
};
use object_store::{ObjectStore as _, ObjectStoreExt};

/// Shared base implemented by [`crate::object_store::LooseObjectStore`] and
/// [`crate::object_store::MemoryObjectStore`].
///
/// It factors out the object-store napi surface (exists / lookup / read / write / stream) so each
/// variant only provides the storage-specific primitives via the `required` methods below; the
/// `default` methods implement the napi-facing glue once.
pub trait ObjectStore {
    /// The hash kind this store uses.
    fn kind(&self) -> gix::hash::Kind;

    /// Whether an object with the given hex `id` exists.
    fn contains_id(&self, id: &str) -> bool;

    /// Resolve a short hex `prefix` to a unique object.
    fn lookup_prefix_impl(&self, prefix: &str) -> napi::Result<PrefixLookup>;

    /// Read an object's body + kind, or `None` if missing.
    fn find_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectData>>;

    /// Read an object's header (kind + size), or `None` if missing.
    fn header_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectHeader>>;

    /// Write a raw object of `kind`, returning its computed id.
    fn write_buf_impl(&self, kind: Kind, data: &[u8]) -> napi::Result<ObjectId>;

    /// Write a raw object under a known `id` (hash not recomputed).
    fn write_buf_with_id_impl(
        &self,
        kind: Kind,
        data: &[u8],
        id: ObjectId,
    ) -> napi::Result<ObjectId>;

    /// The on-disk path of the loose object file for `id` (errors for in-memory stores).
    fn object_path_impl(&self, id: &str) -> napi::Result<String>;

    /// The store's path (or `"memory"` for in-memory stores).
    fn store_path(&self) -> String;

    /// The writer target this store flushes buffered objects into.
    fn write_target(&self) -> WriterTarget;

    /// Load an object fully and return a streaming reader.
    fn read_data_impl(&self, id: &str) -> napi::Result<ObjectStoreReader>;

    /// Insert decoded `(id, (kind, data))` objects (from a pack) into the store.
    fn insert_objects(&self, objects: Vec<(ObjectId, (Kind, Vec<u8>))>) -> napi::Result<()>;

    // --- Shared napi-facing glue (default implementations) ---

    fn contains(&self, id: String) -> bool {
        self.contains_id(&id)
    }

    fn lookup_prefix(&self, prefix: String) -> napi::Result<PrefixLookup> {
        self.lookup_prefix_impl(&prefix)
    }

    fn try_find(&self, id: String) -> napi::Result<Option<crate::object::ObjectData>> {
        self.find_impl(&id)
    }

    fn try_header(&self, id: String) -> napi::Result<Option<crate::object::ObjectHeader>> {
        self.header_impl(&id)
    }

    fn object_path(&self, id: String) -> napi::Result<String> {
        self.object_path_impl(&id)
    }

    fn write_object(&self, kind: String, data: Buffer) -> napi::Result<String> {
        let kind = string_to_kind(&kind)?;
        let id = self.write_buf_impl(kind, &data)?;
        Ok(id.to_string())
    }

    fn write_object_with_id(&self, kind: String, data: Buffer, id: String) -> napi::Result<String> {
        let kind = string_to_kind(&kind)?;
        let oid = parse_oid(&id)?;
        let written = self.write_buf_with_id_impl(kind, &data, oid)?;
        Ok(written.to_string())
    }

    fn path(&self) -> String {
        self.store_path()
    }

    fn object_hash(&self) -> String {
        self.kind().to_string()
    }

    fn object_format(&self) -> String {
        hash_kind_format(self.kind()).to_string()
    }

    fn reader(&self, id: String) -> napi::Result<ObjectStoreReader> {
        self.read_data_impl(&id)
    }

    fn writer(&self, kind: String, size: f64) -> napi::Result<ObjectStoreWriter> {
        let kind = string_to_kind(&kind)?;
        Ok(ObjectStoreWriter {
            target: self.write_target(),
            kind,
            buf: Vec::with_capacity(size as usize),
            expected_size: if size <= 0.0 {
                None
            } else {
                Some(size as usize)
            },
        })
    }

    /// Ingest a packfile `Buffer` (e.g. one produced by
    /// [`crate::object_store::MemoryObjectStore::dump`]) into the store, adding all of its objects.
    /// Objects whose id already exists are overwritten.
    fn import_stream(&self, pack: Buffer) -> napi::Result<()> {
        let objects = decode_pack(&pack, self.kind())?;
        self.insert_objects(objects)
    }
}

/// Decode a packfile `Buffer` into its constituent `(id, (kind, data))` objects. Used by both the
/// memory store's `load`/`import_stream` and the loose store's `import_stream`.
pub(crate) fn decode_pack(
    pack: &[u8],
    kind: gix::hash::Kind,
) -> napi::Result<Vec<(ObjectId, (Kind, Vec<u8>))>> {
    let dir = std::env::temp_dir().join(format!("repo-pack-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).map_err(|e| err("load", e))?;
    let pack_path = dir.join("tmp.pack");
    std::fs::write(&pack_path, pack).map_err(|e| err("load", e))?;
    let file = gix_pack::data::File::at(&pack_path, kind).map_err(|e| err("load", e))?;
    let mut inflate = Inflate {
        state: Decompress::new(),
    };
    let mut cache = gix_pack::cache::Never;
    let mut offset = gix_pack::data::header::SIZE as u64;
    let mut out = Vec::new();
    while offset < file.pack_end() as u64 {
        let entry = file.entry(offset).map_err(|e| err("load", e))?;
        let header_size = entry.header_size();
        let mut data = Vec::new();
        let outcome = file
            .decode_entry(
                entry,
                &mut data,
                &mut inflate,
                &resolve_pack_base,
                &mut cache,
            )
            .map_err(|e| err("load", e))?;
        let mut h = gix::hash::hasher(kind);
        let header = format!("{} {}\0", kind_to_string(outcome.kind), data.len());
        h.update(header.as_bytes());
        h.update(&data);
        let id = h.try_finalize().map_err(|e| err("load", e))?;
        out.push((id, (outcome.kind, data)));
        offset += header_size as u64 + outcome.compressed_size as u64;
    }
    Ok(out)
}

/// Resolver used when decoding a packfile without any base objects available (the in-memory store
/// only ever holds fully-resolved objects, so deltas against external bases are not supported).
fn resolve_pack_base(
    _oid: &gix::hash::oid,
    _out: &mut Vec<u8>,
) -> std::result::Result<Option<gix_pack::data::decode::entry::ResolvedBase>, gix::Error> {
    Ok(None)
}

/// An object store that sits on top of any of `object_store`'s implementations —
/// [`LocalFileSystem`], [`InMemory`], or cloud stores (S3/GCS/Azure) — by holding an
/// `Arc<dyn object_store::ObjectStore>`. It reuses the shared [`ObjectStore`] trait, so its napi
/// surface matches [`crate::object_store::LooseObjectStore`] and [`crate::object_store::MemoryObjectStore`].
///
/// Objects are stored as raw git objects (`"<kind> <size>\0<payload>"`) under a two-character fanout
/// path (`ab/cdef…`), keyed by their content hash — i.e. the same layout git uses, minus the zlib
/// compression. `gitLoose: true` switches the on-disk format to genuine git loose objects
/// (zlib-compressed), byte-compatible with a real `.git/objects` tree.

/// A dedicated tokio runtime used to bridge the `object_store` crate's async API to the
/// synchronous napi methods. The `object_store` backends perform their I/O via blocking `std::fs`
/// calls inside `maybe_spawn_blocking`, so only the scheduler/blocking-pool is required (no IO
/// driver).
pub(crate) fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .build()
            .expect("generic object store: failed to build tokio runtime")
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
    h.try_finalize().map_err(|e| err("generic", e))
}

/// Split the raw git object bytes into `(kind, payload)` by parsing the leading header.
fn parse_loose(bytes: &[u8]) -> napi::Result<(Kind, Vec<u8>)> {
    let nul = bytes
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| err_str("generic", "object bytes missing header NUL terminator"))?;
    let header = &bytes[..nul];
    let sp = header
        .iter()
        .position(|&b| b == b' ')
        .ok_or_else(|| err_str("generic", "object header missing space separator"))?;
    let kind = match &header[..sp] {
        b"blob" => Kind::Blob,
        b"tree" => Kind::Tree,
        b"commit" => Kind::Commit,
        b"tag" => Kind::Tag,
        _ => return Err(err_str("generic", "unknown object kind in header")),
    };
    Ok((kind, bytes[nul + 1..].to_vec()))
}

/// Compress raw git-object bytes into a zlib stream — i.e. the on-disk format of a git loose object.
/// Uses git's default loose compression level (fast).
fn zlib_compress(raw: &[u8]) -> napi::Result<Vec<u8>> {
    use std::io::Write as _;
    let mut w =
        gix::zlib::stream::deflate::Write::new(Vec::new(), gix::zlib::Compression::BEST_SPEED);
    w.write_all(raw).map_err(|e| err("generic_deflate", e))?;
    w.flush().map_err(|e| err("generic_deflate", e))?;
    Ok(w.into_inner())
}

/// Inflate a zlib stream back into the raw git-object bytes.
fn zlib_inflate(compressed: &[u8]) -> napi::Result<Vec<u8>> {
    use std::io::Cursor;
    let mut state = gix::zlib::Inflate::default().state;
    let mut out = Vec::with_capacity(compressed.len().saturating_mul(2).max(4096));
    let mut rd = Cursor::new(compressed);
    loop {
        let len = out.len();
        out.resize(len + 8192, 0);
        let written = gix::zlib::stream::inflate::read(&mut rd, &mut state, &mut out[len..])
            .map_err(|e| err_str("generic_inflate", &e.to_string()))?;
        out.truncate(len + written);
        if written == 0 {
            break;
        }
    }
    Ok(out)
}

/// The storage backend: an `Arc<dyn object_store::ObjectStore>` plus the metadata we need to bridge
/// back to git (hash kind, a human-readable root, and — for local filesystems — the on-disk prefix so
/// we can resolve `object_path`).
pub(crate) struct GenericStore {
    store: Arc<dyn object_store::ObjectStore>,
    kind: gix::hash::Kind,
    root: String,
    fs_prefix: Option<PathBuf>,
    /// When set, objects are stored as zlib-compressed git loose objects (byte-compatible with a
    /// real `.git/objects` directory) instead of the raw, uncompressed git object bytes.
    git_loose: bool,
}

impl GenericStore {
    /// Persist the git object bytes under `id`'s fanout key. In `git_loose` mode the raw object is
    /// zlib-compressed first so the file is byte-compatible with git's loose format.
    fn put(&self, id: &str, raw: Vec<u8>) -> napi::Result<()> {
        let data = if self.git_loose {
            zlib_compress(&raw)?
        } else {
            raw
        };
        let loc = object_store::path::Path::parse(fanout(id)).map_err(|e| err("generic", e))?;
        let payload = object_store::PutPayload::from(data);
        runtime()
            .block_on(self.store.put(&loc, payload))
            .map_err(|e| err("generic_put", e))?;
        Ok(())
    }

    /// Fetch the raw (uncompressed) git object bytes for `id`, or `None` if absent. In `git_loose`
    /// mode the stored bytes are zlib-compressed and are inflated here before being returned.
    fn get(&self, id: &str) -> napi::Result<Option<Vec<u8>>> {
        let loc = object_store::path::Path::parse(fanout(id)).map_err(|e| err("generic", e))?;
        match runtime().block_on(self.store.get(&loc)) {
            Ok(res) => {
                let bytes = runtime()
                    .block_on(res.bytes())
                    .map_err(|e| err("generic_get", e))?;
                let bytes = bytes.to_vec();
                let raw = if self.git_loose {
                    zlib_inflate(&bytes)?
                } else {
                    bytes
                };
                Ok(Some(raw))
            }
            Err(e) if matches!(e, object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(err("generic_get", e)),
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
                Some(Err(e)) => return Err(err("generic_list", e)),
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
fn build(
    uri: &str,
    options: Option<::object_store::client::ClientOptions>,
) -> napi::Result<(Arc<dyn object_store::ObjectStore>, String, Option<PathBuf>)> {
    let trimmed = uri.trim();
    if trimmed.is_empty() || trimmed == "memory" || trimmed.starts_with("memory://") {
        let mem = Arc::new(::object_store::memory::InMemory::new());
        let store: Arc<dyn object_store::ObjectStore> = mem;
        return Ok((store, "memory".to_string(), None));
    }

    // HTTP/HTTPS backend (`object_store::http`). The caller's `ObjectStoreClient` supplies the
    // `ClientOptions` (TLS, proxy, timeouts, HTTP version). SSH is not an object_store transport —
    // git's SSH transport is handled separately by gix.
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        let mut builder = ::object_store::http::HttpBuilder::new().with_url(trimmed.to_string());
        if let Some(options) = options {
            builder = builder.with_client_options(options);
        }
        let store = builder.build().map_err(|e| err("generic_new", e))?;
        let store: Arc<dyn object_store::ObjectStore> = Arc::new(store);
        return Ok((store, trimmed.to_string(), None));
    }

    let path = if let Some(p) = trimmed.strip_prefix("file://") {
        p.to_string()
    } else {
        trimmed.to_string()
    };
    let p = StdPath::new(&path);
    std::fs::create_dir_all(p).map_err(|e| err("generic_new", e))?;
    let canon = std::fs::canonicalize(p).map_err(|e| err("generic_new", e))?;
    let fs = object_store::local::LocalFileSystem::new_with_prefix(&canon)
        .map_err(|e| err("generic_new", e))?;
    let store: Arc<dyn object_store::ObjectStore> = Arc::new(fs);
    let root = canon.to_string_lossy().to_string();
    Ok((store, root, Some(canon)))
}

/// Metadata for a single object, as exposed by [`GenericObjectStore::list`] / [`GenericObjectStore::list_with_delimiter`].
///
/// Mirrors `object_store`'s `ObjectMeta` (location, size, last-modified) and adds the derived `id` (the
/// fanout path `ab/cdef…` with the `/` stripped), which is the object's content hash in our layout.
#[napi(object)]
pub struct ListedObject {
    /// The object's content hash (hex), derived from its fanout `location`.
    pub id: String,
    /// The on-disk key in the backing `object_store` (`ab/cdef…`).
    pub location: String,
    /// The object's size in bytes.
    pub size: f64,
    /// Last-modified time, in milliseconds since the Unix epoch.
    pub last_modified: f64,
}

/// Result of [`GenericObjectStore::list_with_delimiter`], mirroring `object_store`'s `ListResult`.
///
/// `common_prefixes` are the directories (fanout `ab/`, `cd/`, …) beneath the requested prefix,
/// while `objects` are the entries that live directly under it.
#[napi(object)]
pub struct ListResult {
    /// Fanout directory prefixes (e.g. `ab/`) that group objects under the requested prefix.
    pub common_prefixes: Vec<String>,
    /// Objects directly under the requested prefix (empty for our all-nested fanout layout).
    pub objects: Vec<ListedObject>,
}

/// Convert `object_store`'s `ObjectMeta` into our [`ListedObject`], deriving the hex `id` from the fanout path.
fn listed_from(meta: &object_store::ObjectMeta) -> ListedObject {
    let id = meta.location.to_string().replace('/', "");
    let last_modified = meta.last_modified.timestamp_millis() as f64;
    ListedObject {
        id,
        location: meta.location.to_string(),
        size: meta.size as f64,
        last_modified,
    }
}

/// A git object store backed by the Apache `object_store` crate — see the module docs for the
/// rationale. It reuses the shared [`ObjectStore`] trait, so its napi surface matches
/// [`crate::object_store::LooseObjectStore`] and [`crate::object_store::MemoryObjectStore`].
#[napi]
pub struct GenericObjectStore {
    inner: Arc<GenericStore>,
}

impl ObjectStore for GenericObjectStore {
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
                "generic store is not a local filesystem store",
            )),
        }
    }

    fn store_path(&self) -> String {
        self.inner.root.clone()
    }

    fn write_target(&self) -> WriterTarget {
        WriterTarget::Generic(self.inner.clone())
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
impl GenericObjectStore {
    /// Create a new `object_store`-backed object store.
    ///
    /// - `uri` selects the backend: `"memory"` (or `""`) for an in-memory store; a filesystem path
    ///   (optionally prefixed with `file://`) for a local-disk store (`LocalFileSystem`); or an
    ///   `http(s)://` URL for the `object_store::http` backend (a remote object store spoken over
    ///   HTTP — e.g. a MinIO/S3-compatible gateway). Cloud backends (AWS S3, GCS, Azure) require
    ///   enabling the corresponding `object_store` cargo features and are not yet wired here.
    /// - `object_hash` is `"sha1"` (default) or `"sha256"`.
    /// - `client` is an optional [`ObjectStoreClient`] carrying the HTTP client configuration
    ///   (TLS, proxy, timeouts, HTTP version) applied to HTTP/cloud backends. Ignored for
    ///   memory/local backends.
    #[napi(constructor)]
    pub fn new(
        uri: String,
        object_hash: Option<String>,
        git_loose: Option<bool>,
        client: Option<ObjectStoreClient>,
    ) -> napi::Result<Self> {
        let kind = object_kind_from_option(object_hash.as_deref())?;
        let git_loose = git_loose.unwrap_or(false);
        let options = match client {
            Some(c) => Some(c.to_client_options()?),
            None => None,
        };
        let (store, root, fs_prefix) = build(&uri, options)?;
        Ok(Self {
            inner: Arc::new(GenericStore {
                store,
                kind,
                root,
                fs_prefix,
                git_loose,
            }),
        })
    }

    /// Expose the underlying `object_store` backend to sibling napi modules (e.g. `buffered`),
    /// which wrap `object_store::buffered::{BufReader, BufWriter}`.
    pub(crate) fn backend(&self) -> Arc<dyn object_store::ObjectStore> {
        self.inner.store.clone()
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

    /// Whether this store writes git loose objects (zlib-compressed, byte-compatible with a real
    /// `.git/objects` directory) rather than the default raw, uncompressed git-object bytes.
    #[napi(getter)]
    pub fn git_loose(&self) -> bool {
        self.inner.git_loose
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

    /// List every object in the store with its metadata (id, location, size, last-modified).
    ///
    /// Unlike [`entries`](Self::entries), which returns a flat list of ids, this returns `object_store`'s
    /// `ObjectMeta` for each object. The `id` is derived from the fanout `location` (`ab/cdef…`
    /// with the `/` stripped), so it matches the content hash used everywhere else.
    #[napi]
    pub fn list(&self) -> napi::Result<Vec<ListedObject>> {
        let mut stream = self.inner.store.list(None);
        let mut out = Vec::new();
        loop {
            match runtime().block_on(StreamExt::next(&mut stream)) {
                Some(Ok(meta)) => out.push(listed_from(&meta)),
                Some(Err(e)) => return Err(err("generic_list", e)),
                None => break,
            }
        }
        Ok(out)
    }

    /// List objects grouped by the `/` delimiter beneath an optional `prefix`, mirroring `object_store`'s
    /// `ObjectStore::list_with_delimiter`.
    ///
    /// Because every object lives under a two-character fanout directory (`ab/cdef…`), the objects
    /// themselves never appear at the top level: instead `common_prefixes` reports the distinct
    /// fanout directories (`ab/`, `cd/`, …) that fall under `prefix`, while `objects` is empty
    /// unless `prefix` already names a leaf fanout directory.
    #[napi]
    pub fn list_with_delimiter(&self, prefix: Option<String>) -> napi::Result<ListResult> {
        let prefix_path = match prefix {
            Some(p) if !p.is_empty() => {
                Some(object_store::path::Path::parse(&p).map_err(|e| err("generic", e))?)
            }
            _ => None,
        };
        let res = runtime()
            .block_on(self.inner.store.list_with_delimiter(prefix_path.as_ref()))
            .map_err(|e| err("generic_list", e))?;
        Ok(ListResult {
            common_prefixes: res.common_prefixes.iter().map(|p| p.to_string()).collect(),
            objects: res.objects.iter().map(listed_from).collect(),
        })
    }

    /// Remove the object identified by `id` from the store. Errors if the object is absent.
    #[napi]
    pub fn delete(&self, id: String) -> napi::Result<()> {
        let loc = object_store::path::Path::parse(fanout(&id)).map_err(|e| err("generic", e))?;
        runtime()
            .block_on(self.inner.store.delete(&loc))
            .map_err(|e| err("generic_delete", e))?;
        Ok(())
    }
}
