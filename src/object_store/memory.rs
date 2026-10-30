use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use gix::ObjectId;
use gix::object::Kind;
use gix::zlib::Compression;

use crate::error::{err, err_str};
use crate::object::kind_to_string;
use crate::object_store::base::ObjectStore;
use crate::object_store::{
    ObjectStoreReader, ObjectStoreWriter, PrefixLookup, WriterTarget, object_kind_from_option,
};

/// An in-memory object store, mirroring libgit2's `git_mempack`.
///
/// Objects live in a single `Mutex`-guarded map keyed by their id. Backs
/// `new MemoryObjectStore()`, which writes transient objects, then `dump()`s them as a pack or
/// drops the whole store to discard.
pub(crate) struct InMemoryStore {
    objects: Mutex<HashMap<ObjectId, (Kind, Vec<u8>)>>,
    kind: gix::hash::Kind,
}

impl InMemoryStore {
    pub(crate) fn new(kind: gix::hash::Kind) -> Self {
        Self {
            objects: Mutex::new(HashMap::new()),
            kind,
        }
    }
    fn contains(&self, id: &str) -> bool {
        ObjectId::from_hex(id.as_bytes())
            .map(|oid| self.objects.lock().unwrap().contains_key(&oid))
            .unwrap_or(false)
    }
    fn try_find(&self, id: &str) -> napi::Result<Option<crate::object::ObjectData>> {
        let oid = ObjectId::from_hex(id.as_bytes()).map_err(|e| err("parse_oid", e))?;
        let map = self.objects.lock().unwrap();
        Ok(map.get(&oid).map(|(kind, data)| crate::object::ObjectData {
            kind: kind_to_string(*kind),
            data: Buffer::from(data.clone()),
        }))
    }
    fn try_header(&self, id: &str) -> napi::Result<Option<crate::object::ObjectHeader>> {
        let oid = ObjectId::from_hex(id.as_bytes()).map_err(|e| err("parse_oid", e))?;
        let map = self.objects.lock().unwrap();
        Ok(map
            .get(&oid)
            .map(|(kind, data)| crate::object::ObjectHeader {
                kind: kind_to_string(*kind),
                size: data.len() as f64,
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
            .map(|(kind, data)| ObjectStoreReader {
                data: data.clone(),
                kind: kind_to_string(*kind),
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
        self.objects
            .lock()
            .unwrap()
            .insert(id, (kind, data.to_vec()));
        Ok(id)
    }
    fn write_object_with_id(&self, id: ObjectId, kind: Kind, data: &[u8]) -> napi::Result<()> {
        self.objects
            .lock()
            .unwrap()
            .insert(id, (kind, data.to_vec()));
        Ok(())
    }
}

/// A database for reading and writing objects kept entirely in memory, mirroring libgit2's
/// `git_mempack`. Objects live in a single map keyed by their id; `dump` serializes them into a
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
        let mut map = self.inner.objects.lock().unwrap();
        for (id, (kind, data)) in objects {
            map.insert(id, (kind, data));
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

    /// Serialize every object currently held in memory into a git packfile `Buffer`.
    ///
    /// The produced pack can be restored via [`load`](Self::load) or fed to `git unpack-objects`.
    #[napi]
    pub fn dump(&self) -> napi::Result<Buffer> {
        let kind = self.inner.kind;
        let mut entries = Vec::new();
        {
            let map = self.inner.objects.lock().unwrap();
            for (id, (k, data)) in map.iter() {
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
        Ok(())
    }

    /// Return the hex ids of every object currently held in memory, in insertion order.
    #[napi]
    pub fn entries(&self) -> napi::Result<Vec<String>> {
        let map = self.inner.objects.lock().unwrap();
        Ok(map.keys().map(|id| id.to_string()).collect())
    }

    /// Return the number of objects currently held in memory.
    #[napi]
    pub fn size(&self) -> napi::Result<f64> {
        Ok(self.inner.objects.lock().unwrap().len() as f64)
    }
}
