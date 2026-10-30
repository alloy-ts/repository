use std::sync::Mutex;

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use gix::objs::Write;
use gix::zlib::Compression;

use crate::error::{err, parse_oid};
use crate::object::kind_to_string;
use crate::object_store::base::ObjectStore;
use crate::object_store::{
    object_kind_from_option, parse_hash_or_options, ObjectStoreOptions, ObjectStoreReader,
    ObjectStoreWriter, PrefixLookup, WriterTarget,
};

/// A database for reading and writing loose git objects on disk, one file per object.
///
/// Wraps `gix::odb::loose::Store`. This is the low-level, repository-independent object
/// database: it operates directly on an `objects/` directory and does not consult packs,
/// alternates, or the rest of a repository. For the pack-aware store, see `gix::odb::Store`,
/// exposed via [`crate::object_store::ObjectDatabase`]; for a transient in-memory store, see
/// [`crate::object_store::MemoryObjectStore`].
///
/// Implements the shared [`ObjectStore`] base trait.
#[napi]
pub struct LooseObjectStore {
    inner: gix::odb::loose::Store,
}

impl ObjectStore for LooseObjectStore {
    fn kind(&self) -> gix::hash::Kind {
        self.inner.object_hash()
    }

    fn contains_id(&self, id: &str) -> bool {
        parse_oid(id)
            .map(|oid| self.inner.contains(&oid))
            .unwrap_or(false)
    }

    fn lookup_prefix_impl(&self, prefix: &str) -> napi::Result<PrefixLookup> {
        let prefix = gix::hash::Prefix::from_hex(prefix).map_err(|e| err("lookup_prefix", e))?;
        match self.inner.lookup_prefix(prefix, None) {
            Ok(Some(Ok(oid))) => Ok(PrefixLookup {
                found: true,
                ambiguous: false,
                id: Some(oid.to_string()),
            }),
            Ok(Some(Err(()))) => Ok(PrefixLookup {
                found: false,
                ambiguous: true,
                id: None,
            }),
            Ok(None) => Ok(PrefixLookup {
                found: false,
                ambiguous: false,
                id: None,
            }),
            Err(e) => Err(err("lookup_prefix", e)),
        }
    }

    fn find_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectData>> {
        let oid = parse_oid(id)?;
        let mut buf = Vec::new();
        let data = self
            .inner
            .try_find(&oid, &mut buf)
            .map_err(|e| err("try_find", e))?;
        Ok(data.map(|d| crate::object::ObjectData {
            kind: kind_to_string(d.kind),
            data: Buffer::from(d.data.to_vec()),
        }))
    }

    fn header_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectHeader>> {
        let oid = parse_oid(id)?;
        let header = self
            .inner
            .try_header(&oid)
            .map_err(|e| err("try_header", e))?;
        Ok(header.map(|(size, kind)| crate::object::ObjectHeader {
            kind: kind_to_string(kind),
            size: size as f64,
            is_packed: false,
            num_deltas: None,
        }))
    }

    fn write_buf_impl(&self, kind: gix::object::Kind, data: &[u8]) -> napi::Result<gix::ObjectId> {
        self.inner
            .write_buf(kind, data)
            .map_err(|e| err("write_object", e))
    }

    fn write_buf_with_id_impl(
        &self,
        kind: gix::object::Kind,
        data: &[u8],
        id: gix::ObjectId,
    ) -> napi::Result<gix::ObjectId> {
        gix::objs::Write::write_buf_with_known_id(&self.inner, kind, data, id)
            .map_err(|e| err("write_object_with_id", e))
    }

    fn object_path_impl(&self, id: &str) -> napi::Result<String> {
        let oid = parse_oid(id)?;
        Ok(self.inner.object_path(&oid).to_string_lossy().into_owned())
    }

    fn store_path(&self) -> String {
        self.inner.path().to_string_lossy().into_owned()
    }

    fn write_target(&self) -> WriterTarget {
        WriterTarget::Loose(Mutex::new(self.inner.clone()))
    }

    fn read_data_impl(&self, id: &str) -> napi::Result<ObjectStoreReader> {
        let oid = parse_oid(id)?;
        let mut buf = Vec::new();
        let data = self
            .inner
            .try_find(&oid, &mut buf)
            .map_err(|e| err("reader", e))?;
        match data {
            Some(d) => Ok(ObjectStoreReader {
                data: d.data.to_vec(),
                kind: kind_to_string(d.kind),
                pos: 0,
                max_chunk_size: None,
            }),
            None => Err(crate::error::err_str(
                "reader",
                format!("object {id} not found"),
            )),
        }
    }

    fn insert_objects(
        &self,
        objects: Vec<(gix::ObjectId, (gix::object::Kind, Vec<u8>))>,
    ) -> napi::Result<()> {
        for (id, (kind, data)) in objects {
            gix::objs::Write::write_buf_with_known_id(&self.inner, kind, &data, id)
                .map_err(|e| err("import_stream", e))?;
        }
        Ok(())
    }
}

#[napi]
impl LooseObjectStore {
    /// Open the loose object database rooted at `objects_directory` (typically `<repo>/.git/objects`).
    ///
    /// `object_hash` is `"sha1"` (default) or `"sha256"`. `options` configures the allocation
    /// limit and compression level used when reading/writing loose objects.
    #[napi(constructor)]
    pub fn new(
        objects_directory: String,
        object_hash: Option<napi::bindgen_prelude::Either<String, ObjectStoreOptions>>,
        options: Option<ObjectStoreOptions>,
    ) -> napi::Result<Self> {
        let (hash_str, hash_opts) = parse_hash_or_options(object_hash);
        let kind = object_kind_from_option(hash_str.as_deref())?;
        let options = options.or(hash_opts).unwrap_or(ObjectStoreOptions {
            object_hash: None,
            alloc_limit_bytes: None,
            compression_level: None,
        });
        std::fs::create_dir_all(&objects_directory).map_err(|e| err("LooseObjectStore::new", e))?;
        let compression = match options.compression_level {
            Some(level) => Compression::new(level as i32).unwrap_or(Compression::BEST_SPEED),
            None => Compression::BEST_SPEED,
        };
        let store = gix::odb::loose::Store::at_opts(
            objects_directory,
            kind,
            gix::odb::loose::Options {
                alloc_limit_bytes: options.alloc_limit_bytes.map(|b| b as usize),
                compression,
            },
        );
        Ok(LooseObjectStore { inner: store })
    }

    /// Whether an object with the given id exists as a loose object.
    #[napi]
    pub fn contains(&self, id: String) -> bool {
        ObjectStore::contains(self, id)
    }

    /// Resolve a short object id `prefix` (hex) to a unique object.
    #[napi]
    pub fn lookup_prefix(&self, prefix: String) -> napi::Result<PrefixLookup> {
        ObjectStore::lookup_prefix(self, prefix)
    }

    /// Read an object's raw, decompressed body (without the loose-object header) and kind.
    #[napi]
    pub fn try_find(&self, id: String) -> napi::Result<Option<crate::object::ObjectData>> {
        ObjectStore::try_find(self, id)
    }

    /// Read an object's header (kind + size) without decoding its full content.
    #[napi]
    pub fn try_header(&self, id: String) -> napi::Result<Option<crate::object::ObjectHeader>> {
        ObjectStore::try_header(self, id)
    }

    /// Write a raw object of the given `kind` (`commit`, `tree`, `blob`, `tag`) and return its id.
    #[napi]
    pub fn write_object(&self, kind: String, data: Buffer) -> napi::Result<String> {
        ObjectStore::write_object(self, kind, data)
    }

    /// Return the absolute path of the loose object file for `id` (may not exist yet).
    #[napi]
    pub fn object_path(&self, id: String) -> napi::Result<String> {
        ObjectStore::object_path(self, id)
    }

    /// Return the `objects` directory path.
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

    /// Ingest a packfile `Buffer` (e.g. one produced by
    /// [`crate::object_store::MemoryObjectStore::dump`]) into this store, adding all of its objects.
    #[napi]
    pub fn import_stream(&self, pack: Buffer) -> napi::Result<()> {
        ObjectStore::import_stream(self, pack)
    }
}
