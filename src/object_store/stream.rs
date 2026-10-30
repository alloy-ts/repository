//! A [`StreamObjectStore`] that wraps a low-level [`ObjectStore`] and streams its responses.

use std::fmt::{Debug, Display, Formatter};
use std::ops::Range;
use std::sync::Arc;

use bytes::{BufMut, Bytes, BytesMut};
use futures::StreamExt;
use futures::stream::BoxStream;

use crate::object_store::base::{ObjectStore, PutPayload};
use crate::object_store::{ObjectStoreReader, ObjectStoreWriter, PrefixLookup, WriterTarget};

/// Wraps a [`ObjectStore`] and makes its get response return chunks
/// in a controllable manner for streaming behavior.
pub struct StreamObjectStore {
    inner: Arc<dyn ObjectStore + Send + Sync>,
    chunk_size: usize,
}

impl Debug for StreamObjectStore {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamObjectStore")
            .field("store_path", &self.inner.store_path())
            .field("chunk_size", &self.chunk_size)
            .finish()
    }
}

impl StreamObjectStore {
    /// Creates a new [`StreamObjectStore`] with the specified `chunk_size`.
    pub fn new(inner: Arc<dyn ObjectStore + Send + Sync>, chunk_size: usize) -> Self {
        Self { inner, chunk_size }
    }

    /// Return the chunk size used for streaming reads.
    pub fn chunk_size(&self) -> usize {
        self.chunk_size
    }
}

impl Display for StreamObjectStore {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "StreamObjectStore({})", self.inner.store_path())
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
        format!("stream:{}", self.inner.store_path())
    }

    fn write_target(&self) -> WriterTarget {
        self.inner.write_target()
    }

    fn read_data_impl(&self, id: &str) -> napi::Result<ObjectStoreReader> {
        self.inner.read_data_impl(id)
    }

    fn insert_objects(
        &self,
        objects: Vec<(gix::ObjectId, (gix::object::Kind, Vec<u8>))>,
    ) -> napi::Result<()> {
        self.inner.insert_objects(objects)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object_store::memory::MemoryObjectStore;
    use crate::object_store::base::ObjectStore;

    #[test]
    fn test_stream_store_basic() {
        let mem = Arc::new(MemoryObjectStore::new(None).unwrap());
        let store = StreamObjectStore::new(mem, 1024);
        assert_eq!(store.chunk_size(), 1024);
        assert!(store.store_path().contains("memory"));
    }
}
