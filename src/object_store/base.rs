//! Shared base for the lower-level object stores.
//!
//! [`LooseObjectStore`], [`MemoryObjectStore`], and [`GenericObjectStore`] all implement
//! [`ObjectStore`], which factors out their common napi surface. Each variant supplies only the
//! storage-specific primitives via the *required* trait methods; the *default* methods implement
//! the napi-facing glue (exists / lookup / read / write / stream / `import_stream`) once. The
//! default `import_stream` decodes a packfile via [`decode_pack`] and hands the objects to
//! `insert_objects`.
//!
//! The shared backing implementation is a Store tree over a content-addressed node store
//! (`prolly-map`); no external object-store crate is used.

use std::sync::{Arc, Mutex};

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use chrono::{DateTime, Utc};
use gix::ObjectId;
use gix::object::Kind;
use gix::zlib::{Compression, Decompress, Inflate};
use prolly::{Config as GenericConfig, FileNodeStore, Prolly as Store, Tree as GenericTree};

use crate::error::{err, err_str, parse_oid};
use crate::object::{kind_to_string, string_to_kind};
use crate::object_store::hash_kind_format;
use crate::object_store::{
    ObjectStoreReader, ObjectStoreWriter, PrefixLookup, WriterTarget, object_kind_from_option,
};

/// HTTP protocol version used when a [`crate::object_store::GenericObjectStore`] talks to a
/// remote HTTP-backed object store.
#[napi(string_enum)]
pub enum HttpVersion {
    /// Only attempt HTTP/1.1.
    Http1Only,
    /// Only attempt HTTP/2.
    Http2Only,
    /// Let the client negotiate either version.
    Either,
}

/// Shared base implemented by [`crate::object_store::LooseObjectStore`],
/// [`crate::object_store::MemoryObjectStore`], and [`crate::object_store::GenericObjectStore`].
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

    /// The on-disk path of the loose object file for `id` (errors for in-memory/store stores).
    fn object_path_impl(&self, id: &str) -> napi::Result<String>;

    /// The store's path (or `"memory"`/`"<dir>"` for in-memory/store stores).
    fn store_path(&self) -> String;

    /// The writer target this store flushes buffered objects into.
    fn write_target(&self) -> WriterTarget;

    /// Load an object fully and return a streaming reader.
    fn read_data_impl(&self, id: &str) -> napi::Result<ObjectStoreReader>;

    /// Insert decoded `(id, (kind, data))` objects (from a pack) into the store.
    fn insert_objects(&self, objects: Vec<(ObjectId, (Kind, Vec<u8>))>) -> napi::Result<()>;

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

    /// Ingest a packfile `Buffer` into the store, adding all of its objects.
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

// ===========================================================================
// Store-backed object store (the shared, external-crate-free implementation).
// ===========================================================================

/// Name under which the current tree root is persisted in the node store's
/// `roots/` namespace, so the store survives process restarts. Mirrors the
/// `named_root` convention used by [`crate::tree::open_generic`].
const HEAD_NAME: &[u8] = b"head";

// --- `(kind, data, last_modified)` <-> bytes encoding -----------------------
//
// `last_modified` is the insertion/last-write time, mirroring the `Entry` tracked
// by `InMemoryStore`. Store values are opaque bytes, so we encode the three
// fields ourselves: 1 tag byte for the git object kind, 8 little-endian bytes
// for the `last_modified` timestamp in milliseconds, then the raw object bytes.

const KIND_BLOB: u8 = 0;
const KIND_TREE: u8 = 1;
const KIND_COMMIT: u8 = 2;
const KIND_TAG: u8 = 3;

fn kind_to_byte(k: Kind) -> u8 {
    match k {
        Kind::Blob => KIND_BLOB,
        Kind::Tree => KIND_TREE,
        Kind::Commit => KIND_COMMIT,
        Kind::Tag => KIND_TAG,
    }
}

fn byte_to_kind(b: u8) -> napi::Result<Kind> {
    Ok(match b {
        KIND_BLOB => Kind::Blob,
        KIND_TREE => Kind::Tree,
        KIND_COMMIT => Kind::Commit,
        KIND_TAG => Kind::Tag,
        _ => return Err(err_str("store", format!("unknown object kind tag {b}"))),
    })
}

fn encode_entry(kind: Kind, data: &[u8], last_modified: DateTime<Utc>) -> Vec<u8> {
    let mut v = Vec::with_capacity(1 + 8 + data.len());
    v.push(kind_to_byte(kind));
    v.extend_from_slice(&last_modified.timestamp_millis().to_le_bytes());
    v.extend_from_slice(data);
    v
}

fn decode_entry(bytes: &[u8]) -> napi::Result<(Kind, Vec<u8>, DateTime<Utc>)> {
    if bytes.len() < 9 {
        return Err(err_str("store", "corrupt stored object entry (too short)"));
    }
    let kind = byte_to_kind(bytes[0])?;
    let millis = i64::from_le_bytes(bytes[1..9].try_into().unwrap());
    let last_modified = DateTime::from_timestamp_millis(millis)
        .ok_or_else(|| err_str("store", "stored object entry has invalid timestamp"))?;
    Ok((kind, bytes[9..].to_vec(), last_modified))
}

/// A git object store whose backing map is a prolly tree (`prolly-map`) over a
/// content-addressed, chunked [`FileNodeStore`].
///
/// Objects are keyed by their hex `ObjectId` (the git content hash), and the
/// value is the `(kind, data, last_modified)` triple encoded above. Because the
/// prolly tree is a persistent, immutable, copy-on-write structure:
///
/// - [`fork`](Self::fork) is `O(1)` and shares every underlying node with the
///   parent (the node store is referenced, not copied), unlike
///   `MemoryObjectStore::fork` which deep-copies the whole map.
/// - Identical subtrees across forks/snapshots are deduplicated on disk.
/// - The current root is persisted under [`HEAD_NAME`] via [`save_root`](Self::save_root)
///   and reloaded on construction, so the store is durable across restarts.
///
/// The trade-offs versus a plain `Mutex<HashMap>` (see `memory.rs`): every read
/// (de)serialises the value and is `O(log n)` over the B-tree, and the key is a
/// 40/64-byte hex string rather than the raw 20/32-byte hash. Those costs buy
/// COW forking, on-disk persistence, and cross-state dedup — valuable for a
/// versioned/branchable store, irrelevant for a throwaway mempack.
pub(crate) struct GenericStore {
    /// Cloned (cheaply — see `FileNodeStore`'s `Arc` internals) to build a fresh
    /// `Store` driver for a fork that still shares the same on-disk nodes.
    node_store: FileNodeStore,
    store: Store<FileNodeStore>,
    /// The immutable, copy-on-write tree root. Every write derives a new root.
    tree: Mutex<GenericTree>,
    /// Insertion-order index, kept alongside the tree so `entries()` matches
    /// `MemoryObjectStore`'s contract (Store range order would be hash-sorted).
    order: Mutex<Vec<ObjectId>>,
    kind: gix::hash::Kind,
    /// Directory holding the node store (the store's human-readable path).
    dir: String,
}

impl GenericStore {
    pub(crate) fn new(dir: &str, kind: gix::hash::Kind) -> napi::Result<Self> {
        std::fs::create_dir_all(dir).map_err(|e| err("generic_open", e))?;
        let node_store = FileNodeStore::open(dir).map_err(|e| err("generic_open", e))?;
        let store = Store::new(node_store.clone(), GenericConfig::default());
        let tree = match store
            .load_named_root(HEAD_NAME)
            .map_err(|e| err("generic_load_root", e))?
        {
            Some(t) => t,
            None => store.create(),
        };
        let order = Self::rebuild_order(&store, &tree)?;
        Ok(Self {
            node_store,
            store,
            tree: Mutex::new(tree),
            order: Mutex::new(order),
            kind,
            dir: dir.to_string(),
        })
    }

    /// Rebuild the insertion-order index from a tree by scanning it (used after a
    /// restore/load, where insertion order is unrecoverable → hash order).
    fn rebuild_order(
        store: &Store<FileNodeStore>,
        tree: &GenericTree,
    ) -> napi::Result<Vec<ObjectId>> {
        let mut ids = Vec::new();
        if tree.root.is_none() {
            return Ok(ids);
        }
        for item in store
            .range(tree, b"", None)
            .map_err(|e| err("generic_range", e))?
        {
            let (key, _value) = item.map_err(|e| err("generic_range", e))?;
            ids.push(ObjectId::from_hex(&key).map_err(|e| err("generic_oid", e))?);
        }
        Ok(ids)
    }

    /// Insert (or overwrite) an object. The tree derives a new root; new ids are
    /// appended to the insertion-order index.
    fn insert_entry(&self, id: ObjectId, kind: Kind, data: Vec<u8>) -> napi::Result<()> {
        let value = encode_entry(kind, &data, Utc::now());
        let new_tree = {
            let tree = self.tree.lock().unwrap();
            self.store
                .put(&tree, id.to_string().into_bytes(), value)
                .map_err(|e| err("generic_put", e))?
        };
        *self.tree.lock().unwrap() = new_tree;
        let mut order = self.order.lock().unwrap();
        if !order.contains(&id) {
            order.push(id);
        }
        Ok(())
    }

    fn get_value(&self, id: &ObjectId) -> napi::Result<Option<Vec<u8>>> {
        let tree = self.tree.lock().unwrap();
        self.store
            .get(&tree, id.to_string().as_bytes())
            .map_err(|e| err("generic_get", e))
    }

    fn contains(&self, id: &str) -> bool {
        match ObjectId::from_hex(id.as_bytes()) {
            Ok(oid) => self.get_value(&oid).map(|v| v.is_some()).unwrap_or(false),
            Err(_) => false,
        }
    }

    fn last_modified(&self, id: &str) -> napi::Result<Option<DateTime<Utc>>> {
        let oid = parse_oid(id)?;
        match self.get_value(&oid)? {
            None => Ok(None),
            Some(value) => {
                let (_kind, _data, lm) = decode_entry(&value)?;
                Ok(Some(lm))
            }
        }
    }

    fn try_find(&self, id: &str) -> napi::Result<Option<crate::object::ObjectData>> {
        let oid = parse_oid(id)?;
        match self.get_value(&oid)? {
            None => Ok(None),
            Some(value) => {
                let (kind, data, _lm) = decode_entry(&value)?;
                Ok(Some(crate::object::ObjectData {
                    kind: kind_to_string(kind),
                    data: Buffer::from(data),
                }))
            }
        }
    }

    fn try_header(&self, id: &str) -> napi::Result<Option<crate::object::ObjectHeader>> {
        let oid = parse_oid(id)?;
        match self.get_value(&oid)? {
            None => Ok(None),
            Some(value) => {
                let (kind, data, _lm) = decode_entry(&value)?;
                Ok(Some(crate::object::ObjectHeader {
                    kind: kind_to_string(kind),
                    size: data.len() as f64,
                    is_packed: false,
                    num_deltas: None,
                }))
            }
        }
    }

    fn lookup_prefix(&self, prefix: &str) -> napi::Result<PrefixLookup> {
        let p = prefix.to_ascii_lowercase();
        let mut found: Option<ObjectId> = None;
        let mut count = 0usize;
        let tree = self.tree.lock().unwrap();
        if tree.root.is_some() {
            for item in self
                .store
                .range(&tree, b"", None)
                .map_err(|e| err("generic_range", e))?
            {
                let (key, _value) = item.map_err(|e| err("generic_range", e))?;
                let oid = match ObjectId::from_hex(&key) {
                    Ok(o) => o,
                    Err(_) => continue,
                };
                if oid.to_string().starts_with(&p) {
                    count += 1;
                    found = Some(oid);
                }
            }
        }
        Ok(match found {
            Some(oid) if count == 1 => PrefixLookup {
                found: true,
                ambiguous: false,
                id: Some(oid.to_string()),
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

    fn reader(&self, id: &str) -> napi::Result<ObjectStoreReader> {
        let oid = parse_oid(id)?;
        match self.get_value(&oid)? {
            None => Err(err_str("reader", format!("object {id} not found"))),
            Some(value) => {
                let (kind, data, _lm) = decode_entry(&value)?;
                Ok(ObjectStoreReader {
                    data,
                    kind: kind_to_string(kind),
                    pos: 0,
                })
            }
        }
    }

    pub(crate) fn write_object(&self, kind: Kind, data: &[u8]) -> napi::Result<ObjectId> {
        let mut h = gix::hash::hasher(self.kind);
        let header = format!("{} {}\0", kind_to_string(kind), data.len());
        h.update(header.as_bytes());
        h.update(data);
        let id = h.try_finalize().map_err(|e| err("write_object", e))?;
        self.insert_entry(id, kind, data.to_vec())?;
        Ok(id)
    }

    fn write_object_with_id(
        &self,
        id: ObjectId,
        kind: Kind,
        data: &[u8],
    ) -> napi::Result<ObjectId> {
        self.insert_entry(id, kind, data.to_vec())?;
        Ok(id)
    }

    fn insert_objects(&self, objects: Vec<(ObjectId, (Kind, Vec<u8>))>) -> napi::Result<()> {
        for (id, (kind, data)) in objects {
            self.insert_entry(id, kind, data)?;
        }
        Ok(())
    }

    /// Every object as `(id, kind, data)`, in tree order. Used by `dump`.
    fn all_entries(&self) -> napi::Result<Vec<(ObjectId, Kind, Vec<u8>)>> {
        let tree = self.tree.lock().unwrap();
        let mut out = Vec::new();
        if tree.root.is_none() {
            return Ok(out);
        }
        for item in self
            .store
            .range(&tree, b"", None)
            .map_err(|e| err("generic_range", e))?
        {
            let (key, value) = item.map_err(|e| err("generic_range", e))?;
            let oid = ObjectId::from_hex(&key).map_err(|e| err("generic_oid", e))?;
            let (kind, data, _lm) = decode_entry(&value)?;
            out.push((oid, kind, data));
        }
        Ok(out)
    }

    /// Persist the current root under [`HEAD_NAME`] so a later `new` reloads it.
    fn save_root(&self) -> napi::Result<()> {
        let tree = self.tree.lock().unwrap();
        self.store
            .publish_named_root(HEAD_NAME, &tree)
            .map_err(|e| err("generic_save_root", e))
    }

    /// Reload the persisted root, rebuilding the insertion-order index from it.
    fn restore_root(&self) -> napi::Result<()> {
        let tree = self
            .store
            .load_named_root(HEAD_NAME)
            .map_err(|e| err("generic_restore_root", e))?
            .ok_or_else(|| err_str("restore_root", "no saved root named 'head'"))?;
        let order = Self::rebuild_order(&self.store, &tree)?;
        *self.tree.lock().unwrap() = tree;
        *self.order.lock().unwrap() = order;
        Ok(())
    }

    /// Clear the store: a fresh empty tree and a cleared index. The persisted root
    /// is also dropped (its content-addressed nodes remain for the store's own GC).
    fn reset(&self) -> napi::Result<()> {
        let empty = self.store.create();
        *self.tree.lock().unwrap() = empty;
        self.order.lock().unwrap().clear();
        let _ = self.store.delete_named_root(HEAD_NAME);
        Ok(())
    }

    /// Cheap, copy-on-write fork: clone the node store (shares on-disk nodes),
    /// the current root, and the insertion-order index. Mutations to either side
    /// derive independent roots and do not affect the other.
    fn fork(&self) -> Self {
        let node_store = self.node_store.clone();
        let store = Store::new(node_store.clone(), GenericConfig::default());
        let tree = self.tree.lock().unwrap().clone();
        let order = self.order.lock().unwrap().clone();
        Self {
            node_store,
            store,
            tree: Mutex::new(tree),
            order: Mutex::new(order),
            kind: self.kind,
            dir: self.dir.clone(),
        }
    }
}

/// An object store backed by a Store tree over a content-addressed node store.
///
/// This is the shared, external-crate-free implementation used by
/// [`crate::object_store::LooseObjectStore`] and [`crate::object_store::BufferedObjectStore`].
/// It implements the shared [`ObjectStore`] base trait, so its napi surface matches
/// [`crate::object_store::MemoryObjectStore`].
#[napi]
#[derive(Clone)]
pub struct GenericObjectStore {
    inner: Arc<GenericStore>,
}

impl ObjectStore for GenericObjectStore {
    fn kind(&self) -> gix::hash::Kind {
        self.inner.kind
    }

    fn contains_id(&self, id: &str) -> bool {
        self.inner.contains(id)
    }

    fn lookup_prefix_impl(&self, prefix: &str) -> napi::Result<PrefixLookup> {
        self.inner.lookup_prefix(prefix)
    }

    fn find_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectData>> {
        self.inner.try_find(id)
    }

    fn header_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectHeader>> {
        self.inner.try_header(id)
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
        self.inner.write_object_with_id(id, kind, data)
    }

    fn object_path_impl(&self, _id: &str) -> napi::Result<String> {
        Err(err_str(
            "object_path",
            "store-backed store has no loose-object file path",
        ))
    }

    fn store_path(&self) -> String {
        self.inner.dir.clone()
    }

    fn write_target(&self) -> WriterTarget {
        WriterTarget::Store(self.inner.clone())
    }

    fn read_data_impl(&self, id: &str) -> napi::Result<ObjectStoreReader> {
        self.inner.reader(id)
    }

    fn insert_objects(&self, objects: Vec<(ObjectId, (Kind, Vec<u8>))>) -> napi::Result<()> {
        self.inner.insert_objects(objects)
    }
}

#[napi]
impl GenericObjectStore {
    /// Create a new Store-backed object store rooted at `dir` (created if absent).
    /// `object_hash` is `"sha1"` (default) or `"sha256"`.
    ///
    /// If a previous store was saved at `dir` (via [`save_root`](Self::save_root)),
    /// its root is reloaded automatically; otherwise the store starts empty.
    #[napi(constructor)]
    pub fn new(dir: String, object_hash: Option<String>) -> napi::Result<Self> {
        let kind = object_kind_from_option(object_hash.as_deref())?;
        Ok(GenericObjectStore {
            inner: Arc::new(GenericStore::new(&dir, kind)?),
        })
    }

    /// Whether an object with the given id exists.
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

    /// In-memory stores have no on-disk path; this always errors.
    #[napi]
    pub fn object_path(&self, _id: String) -> napi::Result<String> {
        ObjectStore::object_path(self, _id)
    }

    /// The node-store directory this store is rooted at.
    #[napi(getter)]
    pub fn path(&self) -> String {
        ObjectStore::path(self)
    }

    /// Return the hash kind this store uses (`"sha1"` or `"sha256"`).
    #[napi(getter)]
    pub fn object_hash(&self) -> String {
        ObjectStore::object_hash(self)
    }

    /// The object format this store uses, lower-cased (`"sha1"` or `"sha256"`).
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

    /// Ingest a packfile `Buffer` (e.g. one produced by [`dump`](Self::dump)) into the store,
    /// adding all of its objects. Objects whose id already exists are overwritten.
    #[napi]
    pub fn import_stream(&self, pack: Buffer) -> napi::Result<()> {
        ObjectStore::import_stream(self, pack)
    }

    /// Serialise every object currently held in the store into a git packfile `Buffer`.
    ///
    /// The produced pack can be restored via [`load`](Self::load) or fed to `git unpack-objects`.
    #[napi]
    pub fn dump(&self) -> napi::Result<Buffer> {
        let kind = self.inner.kind;
        let entries = self.inner.all_entries()?;
        let mut pack_entries = Vec::new();
        for (id, k, data) in &entries {
            let count = gix_pack::data::output::Count {
                id: *id,
                entry_pack_location: gix_pack::data::output::count::PackLocation::NotLookedUp,
            };
            let obj = gix::objs::Data {
                kind: *k,
                object_hash: kind,
                data: data.as_slice(),
            };
            let entry =
                gix_pack::data::output::Entry::from_data(&count, &obj, Compression::DEFAULT)
                    .map_err(|e| err("dump", e))?;
            pack_entries.push(entry);
        }
        let num = pack_entries.len() as u32;
        let mut out = Vec::new();
        let iter = std::iter::once(Ok(pack_entries));
        let mut writer = gix_pack::data::output::bytes::FromEntriesIter::new(
            iter,
            &mut out,
            num,
            gix_pack::data::Version::V2,
            kind,
        );
        while let Some(res) = writer.next() {
            res.map_err(|e| err("dump", e))?;
        }
        Ok(Buffer::from(out))
    }

    /// Load a packfile `Buffer` (e.g. one produced by [`dump`](Self::dump)) into the store,
    /// adding all of its objects. Objects whose id already exists are overwritten.
    #[napi]
    pub fn load(&self, pack: Buffer) -> napi::Result<()> {
        ObjectStore::import_stream(self, pack)
    }

    /// Remove every object currently held in the store, leaving it empty.
    /// The persisted root (if any) is also dropped.
    #[napi]
    pub fn reset(&self) -> napi::Result<()> {
        self.inner.reset()
    }

    /// Return the hex ids of every object, in insertion order (hash order after a
    /// restore/load, where insertion order is unrecoverable).
    #[napi]
    pub fn entries(&self) -> napi::Result<Vec<String>> {
        let order = self.inner.order.lock().unwrap();
        Ok(order.iter().map(|id| id.to_string()).collect())
    }

    /// Return the number of objects currently held.
    #[napi]
    pub fn size(&self) -> napi::Result<f64> {
        Ok(self.inner.order.lock().unwrap().len() as f64)
    }

    /// Return the last-modified time (milliseconds since the Unix epoch) of the
    /// object identified by `id`, or `null` if it is not present.
    #[napi]
    pub fn last_modified(&self, id: String) -> napi::Result<Option<f64>> {
        Ok(self
            .inner
            .last_modified(&id)?
            .map(|t| t.timestamp_millis() as f64))
    }

    /// Copy the object identified by `from_id` to a new object stored under `to_id`
    /// (content re-hashed under the new key), returning `to_id`.
    #[napi]
    pub fn copy(&self, from_id: String, to_id: String) -> napi::Result<String> {
        let from = parse_oid(&from_id)?;
        let to = parse_oid(&to_id)?;
        let value = self
            .inner
            .get_value(&from)?
            .ok_or_else(|| err_str("copy", format!("object {from_id} not found")))?;
        let (kind, data, _lm) = decode_entry(&value)?;
        self.inner.write_object_with_id(to, kind, &data)?;
        Ok(to_id)
    }

    /// Return a new, independent store that shares the underlying node store with
    /// this one. Writes to either side derive independent roots (copy-on-write);
    /// the fork is `O(1)` and shares all on-disk nodes — unlike cloning a
    /// `MemoryObjectStore`, which deep-copies the whole map.
    #[napi]
    pub fn fork(&self) -> GenericObjectStore {
        GenericObjectStore {
            inner: Arc::new(self.inner.fork()),
        }
    }

    /// Persist the current tree root under the name `"head"` so a later
    /// [`GenericObjectStore::new`] at the same directory reloads it.
    #[napi]
    pub fn save_root(&self) -> napi::Result<()> {
        self.inner.save_root()
    }

    /// Reload the root previously persisted via [`save_root`](Self::save_root),
    /// replacing the current contents. Errors if no root has been saved.
    #[napi]
    pub fn restore_root(&self) -> napi::Result<()> {
        self.inner.restore_root()
    }
}
