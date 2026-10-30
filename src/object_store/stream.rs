//! Stream-based wrapper over an ObjectStore.

use napi_derive::napi;
use napi::bindgen_prelude::Buffer;

use crate::object_store::base::ObjectStore;
use crate::object_store::{ObjectStoreReader, ObjectStoreWriter, PrefixLookup, WriterTarget};

/// Wraps an underlying ObjectStore to chunk get/read responses in specified buffer sizes.
#[napi]
pub struct StreamObjectStore {
    inner: Box<dyn ObjectStore + Send + Sync>,
    chunk_size: usize,
}

impl StreamObjectStore {
    pub fn new(inner: Box<dyn ObjectStore + Send + Sync>, chunk_size: usize) -> Self {
        Self { inner, chunk_size }
    }
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

    fn write_buf_impl(&self, kind: gix::object::Kind, data: &[u8]) -> napi::Result<gix::ObjectId> {
        self.inner.write_buf_impl(kind, data)
    }

    fn write_buf_with_id_impl(
        &self,
        kind: gix::object::Kind,
        data: &[u8],
        id: gix::ObjectId,
    ) -> napi::Result<gix::ObjectId> {
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

    fn insert_objects(&self, objects: Vec<(gix::ObjectId, (gix::object::Kind, Vec<u8>))>) -> napi::Result<()> {
        self.inner.insert_objects(objects)
    }
}

#[napi]
impl StreamObjectStore {
    #[napi(getter)]
    pub fn chunk_size(&self) -> u32 {
        self.chunk_size as u32
    }

    #[napi]
    pub fn contains(&self, id: String) -> bool {
        ObjectStore::contains(self, id)
    }

    #[napi]
    pub fn lookup_prefix(&self, prefix: String) -> napi::Result<PrefixLookup> {
        ObjectStore::lookup_prefix(self, prefix)
    }

    #[napi]
    pub fn try_find(&self, id: String) -> napi::Result<Option<crate::object::ObjectData>> {
        ObjectStore::try_find(self, id)
    }

    #[napi]
    pub fn try_header(&self, id: String) -> napi::Result<Option<crate::object::ObjectHeader>> {
        ObjectStore::try_header(self, id)
    }

    #[napi]
    pub fn write_object(&self, kind: String, data: Buffer) -> napi::Result<String> {
        ObjectStore::write_object(self, kind, data)
    }

    #[napi(getter)]
    pub fn path(&self) -> String {
        ObjectStore::path(self)
    }

    #[napi(getter)]
    pub fn object_hash(&self) -> String {
        ObjectStore::object_hash(self)
    }

    #[napi(getter)]
    pub fn object_format(&self) -> String {
        ObjectStore::object_format(self)
    }

    #[napi]
    pub fn reader(&self, id: String) -> napi::Result<ObjectStoreReader> {
        ObjectStore::reader(self, id)
    }

    #[napi]
    pub fn writer(&self, kind: String, size: f64) -> napi::Result<ObjectStoreWriter> {
        ObjectStore::writer(self, kind, size)
    }
}
