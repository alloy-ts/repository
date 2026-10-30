//! A [`StreamObjectStore`] that can be used to test streaming behaviour or stream chunks
//! from an underlying [`ObjectStore`](crate::object_store::base::ObjectStore).

use std::sync::Arc;

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use gix::ObjectId;
use gix::object::Kind;

use crate::object_store::base::ObjectStore;
use crate::object_store::{
    ObjectStoreReader, ObjectStoreWriter, PrefixLookup, WriterTarget,
};

/// Wraps an [`ObjectStore`] and delivers chunked reads/streams with a configurable chunk size.
#[napi]
pub struct StreamObjectStore {
    inner: Arc<dyn ObjectStore + Send + Sync>,
    chunk_size: usize,
}

impl ObjectStore for StreamObjectStore {
    fn kind(&self) -> gix::hash::Kind {
        self.inner.kind()
    }

    fn contains_id(&self, id: &str) -> bool {
        self.inner.contains_id(id)
    }

    fn lookup_prefix_impl(&self, prefix: &str) -> napi::Result<PrefixLookup> {
        self.inner.lookup_prefix_impl(prefix)
    }

    fn find_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectData>> {
        self.inner.find_impl(id)
    }

    fn header_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectHeader>> {
        self.inner.header_impl(id)
    }

    fn write_buf_impl(&self, kind: Kind, data: &[u8]) -> napi::Result<ObjectId> {
        self.inner.write_buf_impl(kind, data)
    }

    fn write_buf_with_id_impl(
        &self,
        kind: Kind,
        data: &[u8],
        id: ObjectId,
    ) -> napi::Result<ObjectId> {
        self.inner.write_buf_with_id_impl(kind, data, id)
    }

    fn object_path_impl(&self, id: &str) -> napi::Result<String> {
        self.inner.object_path_impl(id)
    }

    fn store_path(&self) -> String {
        format!("stream({})", self.inner.store_path())
    }

    fn write_target(&self) -> WriterTarget {
        self.inner.write_target()
    }

    fn read_data_impl(&self, id: &str) -> napi::Result<ObjectStoreReader> {
        self.inner.read_data_impl(id)
    }

    fn insert_objects(&self, objects: Vec<(ObjectId, (Kind, Vec<u8>))>) -> napi::Result<()> {
        self.inner.insert_objects(objects)
    }
}

#[napi]
impl StreamObjectStore {
    /// Create a new `StreamObjectStore` with the specified `chunk_size` in bytes.
    #[napi(constructor)]
    pub fn new(chunk_size: Option<u32>) -> Self {
        let memory_store = crate::object_store::memory::MemoryObjectStore::new(None).unwrap();
        Self {
            inner: Arc::new(memory_store),
            chunk_size: chunk_size.unwrap_or(8192) as usize,
        }
    }

    /// Return the configured chunk size in bytes.
    #[napi(getter)]
    pub fn chunk_size(&self) -> f64 {
        self.chunk_size as f64
    }

    /// Whether an object exists in the store.
    #[napi]
    pub fn contains(&self, id: String) -> bool {
        ObjectStore::contains(self, id)
    }

    /// Resolve a short object id prefix.
    #[napi]
    pub fn lookup_prefix(&self, prefix: String) -> napi::Result<PrefixLookup> {
        ObjectStore::lookup_prefix(self, prefix)
    }

    /// Read object data.
    #[napi]
    pub fn try_find(&self, id: String) -> napi::Result<Option<crate::object::ObjectData>> {
        ObjectStore::try_find(self, id)
    }

    /// Read object header.
    #[napi]
    pub fn try_header(&self, id: String) -> napi::Result<Option<crate::object::ObjectHeader>> {
        ObjectStore::try_header(self, id)
    }

    /// Write an object.
    #[napi]
    pub fn write_object(&self, kind: String, data: Buffer) -> napi::Result<String> {
        ObjectStore::write_object(self, kind, data)
    }

    /// Store path description.
    #[napi(getter)]
    pub fn path(&self) -> String {
        ObjectStore::path(self)
    }

    /// Hash kind string.
    #[napi(getter)]
    pub fn object_hash(&self) -> String {
        ObjectStore::object_hash(self)
    }

    /// Format string.
    #[napi(getter)]
    pub fn object_format(&self) -> String {
        ObjectStore::object_format(self)
    }

    /// Open a streaming reader for the object.
    #[napi]
    pub fn reader(&self, id: String) -> napi::Result<ObjectStoreReader> {
        ObjectStore::reader(self, id)
    }

    /// Open a streaming writer for an object.
    #[napi]
    pub fn writer(&self, kind: String, size: f64) -> napi::Result<ObjectStoreWriter> {
        ObjectStore::writer(self, kind, size)
    }
}
