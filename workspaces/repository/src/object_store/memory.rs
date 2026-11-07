use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use chrono::{DateTime, Utc};
use gix::ObjectId;
use gix::object::Kind;
use gix::zlib::Compression;

use crate::error::{err, err_str};
use crate::object::kind_to_string;
use crate::object_store::base::ObjectStore;
use crate::object_store::{
    ObjectStoreReader, ObjectStoreWriter, PrefixLookup, WriterTarget, object_kind_from_option,
};

/// A single stored object plus the metadata we track for it, adapted from the
/// pasted `object_store::memory` `Entry` (which carried `data`, `last_modified`,
/// `attributes`, `e_tag`). For a content-addressed git object store the id *is*
/// the hash, so we keep `last_modified` as the insertion/last-write time and drop
/// the etag/attributes that only apply to a blob store.
#[derive(Clone)]
struct Entry {
    kind: Kind,
    data: Vec<u8>,
    last_modified: DateTime<Utc>,
}

/// An in-memory object store, mirroring libgit2's `git_mempack`.
///
/// Objects live in a single `Mutex`-guarded map keyed by their id, alongside an
/// insertion-order vector so [`entries`](Self::entries) returns a stable,
/// deterministic ordering. Backs `new MemoryObjectStore()`, which writes transient
/// objects, then `dump()`s them as a pack or drops the whole store to discard.
pub(crate) struct InMemoryStore {
    objects: Mutex<HashMap<ObjectId, Entry>>,
    order: Mutex<Vec<ObjectId>>,
    kind: gix::hash::Kind,
}

impl InMemoryStore {
    fn new(kind: gix::hash::Kind) -> Self {
        Self {
            objects: Mutex::new(HashMap::new()),
            order: Mutex::new(Vec::new()),
            kind,
        }
    }

    /// Insert (or overwrite) an object, recording its kind/data and stamping the
    /// current time. New ids are appended to the insertion-order vector; existing
    /// ids keep their original position but refresh their `last_modified`.
    fn insert_entry(&self, id: ObjectId, kind: Kind, data: Vec<u8>) {
        let is_new = {
            let mut map = self.objects.lock().unwrap();
            let is_new = !map.contains_key(&id);
            map.insert(
                id,
                Entry {
                    kind,
                    data,
                    last_modified: Utc::now(),
                },
            );
            is_new
        };
        if is_new {
            self.order.lock().unwrap().push(id);
        }
    }

    fn contains(&self, id: &str) -> bool {
        ObjectId::from_hex(id.as_bytes())
            .map(|oid| self.objects.lock().unwrap().contains_key(&oid))
            .unwrap_or(false)
    }

    fn last_modified(&self, id: &str) -> napi::Result<Option<DateTime<Utc>>> {
        let oid = ObjectId::from_hex(id.as_bytes()).map_err(|e| err("parse_oid", e))?;
        Ok(self
            .objects
            .lock()
            .unwrap()
            .get(&oid)
            .map(|e| e.last_modified))
    }

    fn try_find(&self, id: &str) -> napi::Result<Option<crate::object::ObjectData>> {
        let oid = ObjectId::from_hex(id.as_bytes()).map_err(|e| err("parse_oid", e))?;
        let map = self.objects.lock().unwrap();
        Ok(map.get(&oid).map(|e| crate::object::ObjectData {
            kind: kind_to_string(e.kind),
            data: Buffer::from(e.data.clone()),
        }))
    }

    fn try_header(&self, id: &str) -> napi::Result<Option<crate::object::ObjectHeader>> {
        let oid = ObjectId::from_hex(id.as_bytes()).map_err(|e| err("parse_oid", e))?;
        let map = self.objects.lock().unwrap();
        Ok(map.get(&oid).map(|e| crate::object::ObjectHeader {
            kind: kind_to_string(e.kind),
            size: e.data.len() as f64,
            is_packed: false,
            num_deltas: None,
        }))
    }

    fn lookup_prefix(&self, prefix: &str) -> napi::Result<PrefixLookup> {
        let p = prefix.to_ascii_lowercase();
        let mut found: Option<ObjectId> = None;
        let mut count = 0usize;
        for oid in self.objects.lock().unwrap().keys() {
            if oid.to_string().starts_with(&p) {
                count += 1;
                found = Some(*oid);
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
        let oid = ObjectId::from_hex(id.as_bytes()).map_err(|e| err("parse_oid", e))?;
        let map = self.objects.lock().unwrap();
        map.get(&oid)
            .map(|e| ObjectStoreReader {
                data: e.data.clone(),
                kind: kind_to_string(e.kind),
                pos: 0,
            })
            .ok_or_else(|| err_str("reader", format!("object {id} not found")))
    }

    pub(crate) fn write_object(&self, kind: Kind, data: &[u8]) -> napi::Result<ObjectId> {
        let mut h = gix::hash::hasher(self.kind);
        let header = format!("{} {}\0", kind_to_string(kind), data.len());
        h.update(header.as_bytes());
        h.update(data);
        let id = h.try_finalize().map_err(|e| err("write_object", e))?;
        self.insert_entry(id, kind, data.to_vec());
        Ok(id)
    }

    fn write_object_with_id(&self, id: ObjectId, kind: Kind, data: &[u8]) -> napi::Result<()> {
        self.insert_entry(id, kind, data.to_vec());
        Ok(())
    }

    /// Return a fresh, independent store holding a copy of every object currently
    /// held here, preserving both contents and insertion order. Mutations to the
    /// fork (or the original) do not affect the other. Mirrors the `fork` primitive
    /// exposed by the pasted `object_store::memory` `InMemory` type, adapted to the
    /// git object layout.
    pub(crate) fn fork(&self) -> Self {
        let objects = self.objects.lock().unwrap().clone();
        let order = self.order.lock().unwrap().clone();
        Self {
            objects: Mutex::new(objects),
            order: Mutex::new(order),
            kind: self.kind,
        }
    }
}

/// A database for reading and writing objects kept entirely in memory, mirroring libgit2's
/// `git_mempack`. Objects live in a single map keyed by their id; `dump` serialises them into a
/// pack, and `reset` discards them.
///
/// Implements the shared [`ObjectStore`] base trait.
#[napi]
pub struct MemoryObjectStore {
    inner: Arc<InMemoryStore>,
}

impl ObjectStore for MemoryObjectStore {
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
        self.inner.write_object_with_id(id, kind, data)?;
        Ok(id)
    }

    fn object_path_impl(&self, _id: &str) -> napi::Result<String> {
        Err(err_str(
            "object_path",
            "in-memory object stores have no on-disk path",
        ))
    }

    fn store_path(&self) -> String {
        "memory".to_string()
    }

    fn write_target(&self) -> WriterTarget {
        WriterTarget::Memory(self.inner.clone())
    }

    fn read_data_impl(&self, id: &str) -> napi::Result<ObjectStoreReader> {
        self.inner.reader(id)
    }

    fn insert_objects(&self, objects: Vec<(ObjectId, (Kind, Vec<u8>))>) -> napi::Result<()> {
        for (id, (kind, data)) in objects {
            self.inner.insert_entry(id, kind, data);
        }
        Ok(())
    }
}

#[napi]
impl MemoryObjectStore {
    /// Create a new in-memory object store. `object_hash` is `"sha1"` (default) or `"sha256"`.
    #[napi(constructor)]
    pub fn new(object_hash: Option<String>) -> napi::Result<Self> {
        let kind = object_kind_from_option(object_hash.as_deref())?;
        Ok(MemoryObjectStore {
            inner: Arc::new(InMemoryStore::new(kind)),
        })
    }

    /// Whether an object with the given id exists in memory.
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

    /// Returns the string `"memory"`.
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
    /// adding all of its objects.
    #[napi]
    pub fn import_stream(&self, pack: Buffer) -> napi::Result<()> {
        ObjectStore::import_stream(self, pack)
    }

    /// Serialise every object currently held in memory into a git packfile `Buffer`.
    ///
    /// The produced pack can be restored via [`load`](Self::load) or fed to `git unpack-objects`.
    #[napi]
    pub fn dump(&self) -> napi::Result<Buffer> {
        let kind = self.inner.kind;
        let mut entries = Vec::new();
        {
            let map = self.inner.objects.lock().unwrap();
            for (id, e) in map.iter() {
                let count = gix_pack::data::output::Count {
                    id: *id,
                    entry_pack_location: gix_pack::data::output::count::PackLocation::NotLookedUp,
                };
                let obj = gix::objs::Data {
                    kind: e.kind,
                    object_hash: kind,
                    data: e.data.as_slice(),
                };
                let entry =
                    gix_pack::data::output::Entry::from_data(&count, &obj, Compression::DEFAULT)
                        .map_err(|e| err("dump", e))?;
                entries.push(entry);
            }
        }
        let num = entries.len() as u32;
        let mut out = Vec::new();
        let iter = std::iter::once(Ok(entries));
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

    /// Load a packfile `Buffer` (e.g. one produced by [`dump`](Self::dump)) into the in-memory
    /// store, adding all of its objects. Objects whose id already exists are overwritten.
    #[napi]
    pub fn load(&self, pack: Buffer) -> napi::Result<()> {
        ObjectStore::import_stream(self, pack)
    }

    /// Remove every object currently held in memory, leaving the store empty.
    #[napi]
    pub fn reset(&self) -> napi::Result<()> {
        self.inner.objects.lock().unwrap().clear();
        self.inner.order.lock().unwrap().clear();
        Ok(())
    }

    /// Return the hex ids of every object currently held in memory, in insertion order.
    #[napi]
    pub fn entries(&self) -> napi::Result<Vec<String>> {
        let order = self.inner.order.lock().unwrap();
        Ok(order.iter().map(|id| id.to_string()).collect())
    }

    /// Return the number of objects currently held in memory.
    #[napi]
    pub fn size(&self) -> napi::Result<f64> {
        Ok(self.inner.objects.lock().unwrap().len() as f64)
    }

    /// Return the last-modified time (milliseconds since the Unix epoch) of the
    /// object identified by `id`, or `null` if it is not present. Mirrors the
    /// per-object `last_modified` metadata tracked by the pasted
    /// `object_store::memory` `Entry`, exposed here for parity with the
    /// `GenericObjectStore` listing surface.
    #[napi]
    pub fn last_modified(&self, id: String) -> napi::Result<Option<f64>> {
        Ok(self
            .inner
            .last_modified(&id)?
            .map(|t| t.timestamp_millis() as f64))
    }

    /// Copy the object identified by `from_id` to a new object stored under `to_id`
    /// (without recomputing its hash), returning `to_id`. Errors if `from_id` does
    /// not exist or either id is malformed. Adapted from the pasted
    /// `object_store::memory` `copy_opts` capability, specialised to the git
    /// content-addressed layout, where a "copy" means storing identical content
    /// under an explicit target id.
    #[napi]
    pub fn copy(&self, from_id: String, to_id: String) -> napi::Result<String> {
        let from = ObjectId::from_hex(from_id.as_bytes()).map_err(|e| err("copy", e))?;
        let to = ObjectId::from_hex(to_id.as_bytes()).map_err(|e| err("copy", e))?;
        let (kind, data) = {
            let map = self.inner.objects.lock().unwrap();
            map.get(&from)
                .map(|e| (e.kind, e.data.clone()))
                .ok_or_else(|| err_str("copy", format!("object {from_id} not found")))?
        };
        self.inner.insert_entry(to, kind, data);
        Ok(to_id)
    }

    /// Return a new, independent in-memory store that holds a deep copy of every
    /// object currently in this one. Unlike cloning a `MemoryObjectStore` (which
    /// shares the same backing map), the fork owns its own copy, so writes to the
    /// fork do not appear in the original and vice versa — useful for snapshotting
    /// a transient set of objects before mutating it.
    ///
    /// Adapted from the `fork` method of the `object_store::memory::InMemory` type.
    #[napi]
    pub fn fork(&self) -> MemoryObjectStore {
        MemoryObjectStore {
            inner: Arc::new(self.inner.fork()),
        }
    }
}
