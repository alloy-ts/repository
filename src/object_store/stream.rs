//! A [`StreamObjectStore`] that wraps a base [`ObjectStore`] and yields chunks in controllable sizes.

use std::sync::Arc;

use napi::bindgen_prelude::Buffer;

use gix::object::Kind;
use gix::ObjectId;

use crate::object_store::base::ObjectStore;
use crate::object_store::{
    ObjectStoreReader, PrefixLookup, WriterTarget,
};

/// Wraps an [`ObjectStore`] and produces controllable chunked output for streaming readers.
pub struct StreamObjectStore {
    inner: Arc<dyn ObjectStore + Send + Sync>,
    chunk_size: usize,
}

impl StreamObjectStore {
    /// Creates a new [`StreamObjectStore`] with the specified `chunk_size`.
    pub fn new(inner: Arc<dyn ObjectStore + Send + Sync>, chunk_size: usize) -> Self {
        Self { inner, chunk_size }
    }

    /// Return the configured chunk size.
    pub fn chunk_size(&self) -> usize {
        self.chunk_size
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
        format!("stream://{}", self.inner.store_path())
    }

    fn write_target(&self) -> WriterTarget {
        self.inner.write_target()
    }

    fn read_data_impl(&self, id: &str) -> napi::Result<ObjectStoreReader> {
        self.inner.read_data_impl(id)
    }

    fn insert_objects(
        &self,
        objects: Vec<(ObjectId, (Kind, Vec<u8>))>,
    ) -> napi::Result<()> {
        self.inner.insert_objects(objects)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object_store::memory::MemoryObjectStore;

    #[test]
    fn test_stream_object_store_basic() {
        let mem = Arc::new(MemoryObjectStore::new(None).unwrap());
        let stream_store = StreamObjectStore::new(mem.clone(), 16);

        let data = b"hello stream object store data payload";
        let id = stream_store.write_object("blob".into(), Buffer::from(data.to_vec())).unwrap();

        assert!(stream_store.contains(id.clone()));
        let mut reader = stream_store.reader(id).unwrap();
        assert_eq!(reader.kind(), "blob");

        let mut chunks = Vec::new();
        let mut chunk = reader.read(stream_store.chunk_size() as f64);
        while !chunk.is_empty() {
            chunks.extend_from_slice(&chunk);
            chunk = reader.read(stream_store.chunk_size() as f64);
        }
        assert_eq!(&chunks[..], data);
    }
}
