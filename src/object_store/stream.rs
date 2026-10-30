//! A [`StreamObjectStore`] that wraps another [`ObjectStore`](crate::object_store::base::ObjectStore)
//! and delivers data in controlled chunk sizes.

use std::sync::Arc;

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use gix::object::Kind;
use gix::ObjectId;

use crate::object_store::base::ObjectStore;
use crate::object_store::memory::MemoryObjectStore;
use crate::object_store::{
    parse_hash_or_options, ObjectStoreOptions, ObjectStoreReader, ObjectStoreWriter, PrefixLookup,
    WriterTarget,
};

/// An object database store wrapper that chunks object reading into specified chunk sizes.
#[napi]
pub struct StreamObjectStore {
    inner: Arc<MemoryObjectStore>,
    chunk_size: usize,
}

impl ObjectStore for StreamObjectStore {
    fn kind(&self) -> gix::hash::Kind {
        ObjectStore::kind(self.inner.as_ref())
    }

    fn contains_id(&self, id: &str) -> bool {
        ObjectStore::contains_id(self.inner.as_ref(), id)
    }

    fn lookup_prefix_impl(&self, prefix: &str) -> napi::Result<PrefixLookup> {
        ObjectStore::lookup_prefix_impl(self.inner.as_ref(), prefix)
    }

    fn find_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectData>> {
        ObjectStore::find_impl(self.inner.as_ref(), id)
    }

    fn header_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectHeader>> {
        ObjectStore::header_impl(self.inner.as_ref(), id)
    }

    fn write_buf_impl(&self, kind: Kind, data: &[u8]) -> napi::Result<ObjectId> {
        ObjectStore::write_buf_impl(self.inner.as_ref(), kind, data)
    }

    fn write_buf_with_id_impl(
        &self,
        kind: Kind,
        data: &[u8],
        id: ObjectId,
    ) -> napi::Result<ObjectId> {
        ObjectStore::write_buf_with_id_impl(self.inner.as_ref(), kind, data, id)
    }

    fn object_path_impl(&self, id: &str) -> napi::Result<String> {
        ObjectStore::object_path_impl(self.inner.as_ref(), id)
    }

    fn store_path(&self) -> String {
        format!("stream({})", ObjectStore::store_path(self.inner.as_ref()))
    }

    fn write_target(&self) -> WriterTarget {
        ObjectStore::write_target(self.inner.as_ref())
    }

    fn read_data_impl(&self, id: &str) -> napi::Result<ObjectStoreReader> {
        let mut reader = ObjectStore::read_data_impl(self.inner.as_ref(), id)?;
        reader.max_chunk_size = Some(self.chunk_size);
        Ok(reader)
    }

    fn insert_objects(
        &self,
        objects: Vec<(ObjectId, (Kind, Vec<u8>))>,
    ) -> napi::Result<()> {
        ObjectStore::insert_objects(self.inner.as_ref(), objects)
    }
}

#[napi]
impl StreamObjectStore {
    /// Create a new `StreamObjectStore` wrapping an in-memory store with `chunk_size` bytes.
    #[napi(constructor)]
    pub fn new(
        object_hash: Option<napi::bindgen_prelude::Either<String, ObjectStoreOptions>>,
        chunk_size: Option<u32>,
    ) -> napi::Result<Self> {
        let (hash_str, _) = parse_hash_or_options(object_hash);
        let inner = MemoryObjectStore::new(hash_str.map(napi::bindgen_prelude::Either::A))?;
        Ok(Self {
            inner: Arc::new(inner),
            chunk_size: chunk_size.unwrap_or(64) as usize,
        })
    }

    /// Return the configured chunk size in bytes.
    #[napi(getter)]
    pub fn chunk_size(&self) -> f64 {
        self.chunk_size as f64
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

    /// Return the loose object file path (errors for stream/in-memory store).
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
