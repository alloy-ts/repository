//! Arrow `object_store` adapter for Git object databases.
//!
//! [`ArrowObjectStore`] wraps any implementation of Apache Arrow's [`object_store::ObjectStore`]
//! (e.g. S3, Azure Blob, Google Cloud Storage, LocalFileSystem, InMemory) and implements
//! [`crate::object_store::base::ObjectStore`].
//!
//! Git objects are stored loose at `objects/<aa>/<bbbb...>` relative to the store root, encoded
//! with zlib compression or raw Git header framing (`<kind> <size>\0<content>`).

use std::sync::Arc;

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use futures::StreamExt;
use gix::object::Kind;
use gix::zlib::{Decompress, Inflate};
use gix::ObjectId;
use object_store::path::Path as ArrowPath;
use object_store::{ObjectStore as ArrowStore, ObjectStoreExt};

use crate::error::{err, err_str};
use crate::object::{kind_to_string, string_to_kind};
use crate::object_store::base::ObjectStore;
use crate::object_store::{
    object_kind_from_option, ObjectStoreReader, ObjectStoreWriter, ObjectWriter, PrefixLookup,
    WriterTarget,
};

fn oid_to_arrow_path(id: &str) -> ArrowPath {
    if id.len() >= 2 {
        ArrowPath::from(format!("objects/{}/{}", &id[..2], &id[2..]))
    } else {
        ArrowPath::from(format!("objects/{id}"))
    }
}

fn block_on_future<F: std::future::Future>(future: F) -> F::Output {
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::block_in_place(move || futures::executor::block_on(future))
    } else {
        futures::executor::block_on(future)
    }
}

/// A custom writer handle for [`ArrowObjectStore`].
struct ArrowObjectWriter {
    store: Arc<dyn ArrowStore>,
    kind: gix::hash::Kind,
}

impl ObjectWriter for ArrowObjectWriter {
    fn write_object(&self, kind: Kind, data: &[u8]) -> napi::Result<ObjectId> {
        let mut h = gix::hash::hasher(self.kind);
        let header = format!("{} {}\0", kind_to_string(kind), data.len());
        h.update(header.as_bytes());
        h.update(data);
        let id = h.try_finalize().map_err(|e| err("write_object", e))?;

        let mut payload = Vec::new();
        payload.extend_from_slice(header.as_bytes());
        payload.extend_from_slice(data);

        let arrow_path = oid_to_arrow_path(&id.to_string());
        let store = Arc::clone(&self.store);
        let put_payload = object_store::PutPayload::from(payload);

        let res = block_on_future(async move { store.put(&arrow_path, put_payload).await });
        res.map_err(|e| err("put", e))?;
        Ok(id)
    }
}

/// An object store backend wrapping Apache Arrow's `object_store::ObjectStore`.
///
/// Bridges cloud and local object storage providers (AWS S3, Azure Blob, GCP, Local, InMemory)
/// into a Git loose object database.
#[napi]
pub struct ArrowObjectStore {
    inner: Arc<dyn ArrowStore>,
    kind: gix::hash::Kind,
    store_name: String,
}

impl ArrowObjectStore {
    /// Create a new [`ArrowObjectStore`] wrapping a concrete Arrow `ObjectStore` implementation.
    pub fn new_with_store(
        store: Arc<dyn ArrowStore>,
        kind: gix::hash::Kind,
        name: impl Into<String>,
    ) -> Self {
        Self {
            inner: store,
            kind,
            store_name: name.into(),
        }
    }

    /// Read raw bytes for `id` synchronously via `block_on_future`.
    fn get_bytes_sync(&self, id: &str) -> napi::Result<Option<Vec<u8>>> {
        let arrow_path = oid_to_arrow_path(id);
        let store = Arc::clone(&self.inner);

        let res = block_on_future(async move { store.get(&arrow_path).await });

        match res {
            Ok(get_result) => {
                let bytes_res = block_on_future(async move { get_result.bytes().await });
                let bytes = bytes_res.map_err(|e| err("get_bytes", e))?;
                Ok(Some(bytes.to_vec()))
            }
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(err("get", e)),
        }
    }

    /// Decode git frame (header + payload or zlib decompressed) into `(Kind, Vec<u8>)`.
    fn decode_git_frame(buf: &[u8]) -> napi::Result<(Kind, Vec<u8>)> {
        let data = if let Ok(decompressed) = decompress_zlib(buf) {
            decompressed
        } else {
            buf.to_vec()
        };

        let nul_pos = data
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| err_str("decode", "missing null byte in object header"))?;
        let header_str = std::str::from_utf8(&data[..nul_pos])
            .map_err(|e| err_str("decode", format!("invalid header utf8: {e}")))?;
        let mut parts = header_str.split(' ');
        let kind_str = parts
            .next()
            .ok_or_else(|| err_str("decode", "missing kind in header"))?;
        let kind = string_to_kind(kind_str)?;
        let payload = data[nul_pos + 1..].to_vec();
        Ok((kind, payload))
    }
}

fn decompress_zlib(buf: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let mut inflate = Inflate {
        state: Decompress::new(),
    };
    let mut out = Vec::new();
    inflate.once(buf, &mut out)?;
    Ok(out)
}

impl ObjectStore for ArrowObjectStore {
    fn kind(&self) -> gix::hash::Kind {
        self.kind
    }

    fn contains_id(&self, id: &str) -> bool {
        let arrow_path = oid_to_arrow_path(id);
        let store = Arc::clone(&self.inner);
        block_on_future(async move { store.head(&arrow_path).await }).is_ok()
    }

    fn lookup_prefix_impl(&self, prefix: &str) -> napi::Result<PrefixLookup> {
        let p = prefix.to_ascii_lowercase();
        let prefix_path = ArrowPath::from("objects");
        let store = Arc::clone(&self.inner);

        let list_res = block_on_future(async move {
            let mut stream = store.list(Some(&prefix_path));
            let mut matches = Vec::new();
            while let Some(meta) = stream.next().await {
                if let Ok(meta) = meta {
                    let path_str = meta.location.to_string();
                    let clean_id = path_str
                        .strip_prefix("objects/")
                        .unwrap_or(&path_str)
                        .replace('/', "");
                    if clean_id.starts_with(&p) {
                        matches.push(clean_id);
                    }
                }
            }
            matches
        });

        if list_res.len() == 1 {
            Ok(PrefixLookup {
                found: true,
                ambiguous: false,
                id: Some(list_res[0].clone()),
            })
        } else if list_res.len() > 1 {
            Ok(PrefixLookup {
                found: false,
                ambiguous: true,
                id: None,
            })
        } else {
            Ok(PrefixLookup {
                found: false,
                ambiguous: false,
                id: None,
            })
        }
    }

    fn find_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectData>> {
        match self.get_bytes_sync(id)? {
            Some(raw) => {
                let (kind, data) = Self::decode_git_frame(&raw)?;
                Ok(Some(crate::object::ObjectData {
                    kind: kind_to_string(kind),
                    data: Buffer::from(data),
                }))
            }
            None => Ok(None),
        }
    }

    fn header_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectHeader>> {
        match self.find_impl(id)? {
            Some(obj) => Ok(Some(crate::object::ObjectHeader {
                kind: obj.kind,
                size: obj.data.len() as f64,
                is_packed: false,
                num_deltas: None,
            })),
            None => Ok(None),
        }
    }

    fn write_buf_impl(&self, kind: Kind, data: &[u8]) -> napi::Result<ObjectId> {
        let writer = ArrowObjectWriter {
            store: Arc::clone(&self.inner),
            kind: self.kind,
        };
        writer.write_object(kind, data)
    }

    fn write_buf_with_id_impl(
        &self,
        kind: Kind,
        data: &[u8],
        id: ObjectId,
    ) -> napi::Result<ObjectId> {
        let header = format!("{} {}\0", kind_to_string(kind), data.len());
        let mut payload = Vec::new();
        payload.extend_from_slice(header.as_bytes());
        payload.extend_from_slice(data);

        let arrow_path = oid_to_arrow_path(&id.to_string());
        let store = Arc::clone(&self.inner);
        let put_payload = object_store::PutPayload::from(payload);

        let res = block_on_future(async move { store.put(&arrow_path, put_payload).await });
        res.map_err(|e| err("write_object_with_id", e))?;
        Ok(id)
    }

    fn object_path_impl(&self, id: &str) -> napi::Result<String> {
        Ok(oid_to_arrow_path(id).to_string())
    }

    fn store_path(&self) -> String {
        format!("arrow://{}", self.store_name)
    }

    fn write_target(&self) -> WriterTarget {
        WriterTarget::Custom(Arc::new(ArrowObjectWriter {
            store: Arc::clone(&self.inner),
            kind: self.kind,
        }))
    }

    fn read_data_impl(&self, id: &str) -> napi::Result<ObjectStoreReader> {
        match self.find_impl(id)? {
            Some(obj) => Ok(ObjectStoreReader {
                data: obj.data.to_vec(),
                kind: obj.kind,
                pos: 0,
            }),
            None => Err(err_str("reader", format!("object {id} not found"))),
        }
    }

    fn insert_objects(
        &self,
        objects: Vec<(ObjectId, (Kind, Vec<u8>))>,
    ) -> napi::Result<()> {
        for (id, (kind, data)) in objects {
            self.write_buf_with_id_impl(kind, &data, id)?;
        }
        Ok(())
    }
}

#[napi]
impl ArrowObjectStore {
    /// Create an in-memory Arrow object store instance for Git objects.
    #[napi(factory)]
    pub fn memory(object_hash: Option<String>) -> napi::Result<Self> {
        let kind = object_kind_from_option(object_hash.as_deref())?;
        let in_mem = Arc::new(object_store::memory::InMemory::new());
        Ok(Self::new_with_store(in_mem, kind, "memory"))
    }

    /// Create a local-filesystem Arrow object store instance rooted at `root`.
    #[napi(factory)]
    pub fn local(root: String, object_hash: Option<String>) -> napi::Result<Self> {
        let kind = object_kind_from_option(object_hash.as_deref())?;
        let local_fs = object_store::local::LocalFileSystem::new_with_prefix(&root)
            .map_err(|e| err("local_fs", e))?;
        Ok(Self::new_with_store(Arc::new(local_fs), kind, root))
    }

    /// Whether an object with `id` exists in the store.
    #[napi]
    pub fn contains(&self, id: String) -> bool {
        ObjectStore::contains(self, id)
    }

    /// Resolve a short object id `prefix` (hex) to a unique object.
    #[napi]
    pub fn lookup_prefix(&self, prefix: String) -> napi::Result<PrefixLookup> {
        ObjectStore::lookup_prefix(self, prefix)
    }

    /// Read an object's raw body and kind, or `null` if missing.
    #[napi]
    pub fn try_find(&self, id: String) -> napi::Result<Option<crate::object::ObjectData>> {
        ObjectStore::try_find(self, id)
    }

    /// Read an object's header (kind + size) without decoding full content.
    #[napi]
    pub fn try_header(&self, id: String) -> napi::Result<Option<crate::object::ObjectHeader>> {
        ObjectStore::try_header(self, id)
    }

    /// Write a raw object of `kind` (`commit`, `tree`, `blob`, `tag`) and return its id.
    #[napi]
    pub fn write_object(&self, kind: String, data: Buffer) -> napi::Result<String> {
        ObjectStore::write_object(self, kind, data)
    }

    /// Return the Arrow store relative path for `id`.
    #[napi]
    pub fn object_path(&self, id: String) -> napi::Result<String> {
        ObjectStore::object_path(self, id)
    }

    /// Returns the store path URI.
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

    /// Open a streaming reader for `id`.
    #[napi]
    pub fn reader(&self, id: String) -> napi::Result<ObjectStoreReader> {
        ObjectStore::reader(self, id)
    }

    /// Open a streaming writer for a new object.
    #[napi]
    pub fn writer(&self, kind: String, size: f64) -> napi::Result<ObjectStoreWriter> {
        ObjectStore::writer(self, kind, size)
    }

    /// Write a raw object under an exact `id`.
    #[napi]
    pub fn write_object_with_id(
        &self,
        kind: String,
        data: Buffer,
        id: String,
    ) -> napi::Result<String> {
        ObjectStore::write_object_with_id(self, kind, data, id)
    }

    /// Ingest a packfile `Buffer` into the store.
    #[napi]
    pub fn import_stream(&self, pack: Buffer) -> napi::Result<()> {
        ObjectStore::import_stream(self, pack)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arrow_object_store_memory_basic() {
        let store = ArrowObjectStore::memory(None).unwrap();
        let blob = b"hello arrow object store\n";
        let id = store.write_object("blob".into(), Buffer::from(blob.to_vec())).unwrap();
        assert!(store.contains(id.clone()));

        let found = store.try_find(id.clone()).unwrap().unwrap();
        assert_eq!(found.kind, "blob");
        assert_eq!(found.data.as_ref(), blob);

        let header = store.try_header(id.clone()).unwrap().unwrap();
        assert_eq!(header.kind, "blob");
        assert_eq!(header.size, blob.len() as f64);
    }
}
