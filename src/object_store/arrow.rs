use std::io::{Read, Write};
use std::sync::Arc;

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use futures::stream::StreamExt;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;
use object_store::path::Path as ArrowPath;
use object_store::{ObjectStore as ArrowObjectStoreTrait, ObjectStoreExt as _};

use gix::object::Kind;
use gix::ObjectId;

use crate::error::{err, err_str};
use crate::object::kind_to_string;
use crate::object_store::base::ObjectStore;
use crate::object_store::{
    object_kind_from_option, ObjectStoreOptions, ObjectStoreReader, ObjectStoreWriter,
    PrefixLookup, WriterTarget,
};

/// Helper to execute async `object_store` futures synchronously within NAPI methods.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(future))
    } else {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime");
        rt.block_on(future)
    }
}

fn decompress_zlib(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut decoder = ZlibDecoder::new(bytes);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out)?;
    Ok(out)
}

fn compress_zlib(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(bytes)?;
    encoder.finish()
}

/// Inner wrapper over any [`object_store::ObjectStore`] implementation (such as
/// `object_store::local::LocalFileSystem` or `object_store::memory::InMemory`).
pub(crate) struct ArrowOdbStore {
    inner: Arc<dyn ArrowObjectStoreTrait>,
    kind: gix::hash::Kind,
    prefix: String,
}

impl ArrowOdbStore {
    pub(crate) fn new(
        inner: Arc<dyn ArrowObjectStoreTrait>,
        kind: gix::hash::Kind,
        prefix: Option<String>,
    ) -> Self {
        let prefix = prefix.unwrap_or_default();
        Self { inner, kind, prefix }
    }

    fn object_key(&self, id: &str) -> ArrowPath {
        let clean_prefix = self.prefix.trim_matches('/');
        let path_str = if id.len() >= 2 {
            let (dir, file) = id.split_at(2);
            if clean_prefix.is_empty() {
                format!("{dir}/{file}")
            } else {
                format!("{clean_prefix}/{dir}/{file}")
            }
        } else if clean_prefix.is_empty() {
            id.to_string()
        } else {
            format!("{clean_prefix}/{id}")
        };
        ArrowPath::from(path_str)
    }

    fn parse_object_data(bytes: &[u8]) -> napi::Result<(Kind, Vec<u8>)> {
        if let Ok(decompressed) = decompress_zlib(bytes) {
            if let Some(nul_pos) = decompressed.iter().position(|b| *b == 0) {
                let header = std::str::from_utf8(&decompressed[..nul_pos])
                    .map_err(|e| err_str("parse_header", e.to_string()))?;
                let mut parts = header.split_whitespace();
                if let (Some(kind_str), Some(_size_str)) = (parts.next(), parts.next()) {
                    let kind = crate::object::string_to_kind(kind_str)?;
                    let data = decompressed[nul_pos + 1..].to_vec();
                    return Ok((kind, data));
                }
            }
        }
        Err(err_str("parse_object_data", "invalid loose object data"))
    }

    fn read_object_bytes(&self, id: &str) -> napi::Result<Option<Vec<u8>>> {
        let key = self.object_key(id);
        let fut = async {
            match self.inner.get(&key).await {
                Ok(get_res) => match get_res.bytes().await {
                    Ok(bytes) => Ok(Some(bytes.to_vec())),
                    Err(e) => Err(err("read_bytes", e)),
                },
                Err(object_store::Error::NotFound { .. }) => Ok(None),
                Err(e) => Err(err("get", e)),
            }
        };
        block_on(fut)
    }

    pub(crate) fn contains(&self, id: &str) -> bool {
        let key = self.object_key(id);
        block_on(async {
            match self.inner.head(&key).await {
                Ok(_) => true,
                Err(_) => false,
            }
        })
    }

    pub(crate) fn try_find(&self, id: &str) -> napi::Result<Option<crate::object::ObjectData>> {
        if let Some(raw) = self.read_object_bytes(id)? {
            let (kind, data) = Self::parse_object_data(&raw)?;
            Ok(Some(crate::object::ObjectData {
                kind: kind_to_string(kind),
                data: Buffer::from(data),
            }))
        } else {
            Ok(None)
        }
    }

    pub(crate) fn try_header(&self, id: &str) -> napi::Result<Option<crate::object::ObjectHeader>> {
        if let Some(raw) = self.read_object_bytes(id)? {
            let (kind, data) = Self::parse_object_data(&raw)?;
            Ok(Some(crate::object::ObjectHeader {
                kind: kind_to_string(kind),
                size: data.len() as f64,
                is_packed: false,
                num_deltas: None,
            }))
        } else {
            Ok(None)
        }
    }

    pub(crate) fn write_object(&self, kind: Kind, data: &[u8]) -> napi::Result<ObjectId> {
        let mut h = gix::hash::hasher(self.kind);
        let header = format!("{} {}\0", kind_to_string(kind), data.len());
        h.update(header.as_bytes());
        h.update(data);
        let id = h.try_finalize().map_err(|e| err("write_object", e))?;

        self.write_object_with_id(id, kind, data)?;
        Ok(id)
    }

    pub(crate) fn write_object_with_id(
        &self,
        id: ObjectId,
        kind: Kind,
        data: &[u8],
    ) -> napi::Result<()> {
        let id_str = id.to_string();
        let key = self.object_key(&id_str);

        let header = format!("{} {}\0", kind_to_string(kind), data.len());
        let mut uncompressed = Vec::with_capacity(header.len() + data.len());
        uncompressed.extend_from_slice(header.as_bytes());
        uncompressed.extend_from_slice(data);

        let compressed = compress_zlib(&uncompressed).map_err(|e| err("compress_zlib", e))?;
        let payload = object_store::PutPayload::from(compressed);

        block_on(async {
            self.inner
                .put(&key, payload)
                .await
                .map_err(|e| err("put", e))?;
            Ok(())
        })
    }

    pub(crate) fn lookup_prefix(&self, prefix: &str) -> napi::Result<PrefixLookup> {
        let p = prefix.to_ascii_lowercase();
        let clean_prefix = self.prefix.trim_matches('/');
        let search_prefix = if clean_prefix.is_empty() {
            if p.len() >= 2 {
                format!("{}/", &p[..2])
            } else {
                String::new()
            }
        } else if p.len() >= 2 {
            format!("{clean_prefix}/{}/", &p[..2])
        } else {
            format!("{clean_prefix}/")
        };

        let path = if search_prefix.is_empty() {
            None
        } else {
            Some(ArrowPath::from(search_prefix.as_str()))
        };

        let fut = async {
            let mut stream = self.inner.list(path.as_ref());
            let mut matches = Vec::new();
            while let Some(meta_res) = stream.next().await {
                if let Ok(meta) = meta_res {
                    let loc_str = meta.location.to_string();
                    let relative = if clean_prefix.is_empty() {
                        loc_str.as_str()
                    } else {
                        loc_str.strip_prefix(clean_prefix).unwrap_or(&loc_str).trim_start_matches('/')
                    };
                    let oid_str = relative.replace('/', "");
                    if oid_str.starts_with(&p) {
                        matches.push(oid_str);
                    }
                }
            }
            Ok::<Vec<String>, napi::Error>(matches)
        };

        let matches: Vec<String> = block_on(fut)?;
        if matches.len() == 1 {
            Ok(PrefixLookup {
                found: true,
                ambiguous: false,
                id: Some(matches[0].clone()),
            })
        } else if matches.len() > 1 {
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

    pub(crate) fn reader(&self, id: &str) -> napi::Result<ObjectStoreReader> {
        if let Some(raw) = self.read_object_bytes(id)? {
            let (kind, data) = Self::parse_object_data(&raw)?;
            Ok(ObjectStoreReader {
                data,
                kind: kind_to_string(kind),
                pos: 0,
            })
        } else {
            Err(err_str("reader", format!("object {id} not found")))
        }
    }
}

/// An object store backend wrapping Apache Arrow's `object_store::ObjectStore` crate interface.
///
/// Can operate on local disk, in-memory, or cloud object stores (S3, Azure, GCP, etc.) transparently.
/// Implements the shared [`ObjectStore`] base trait.
#[napi]
pub struct ArrowObjectStore {
    pub(crate) inner: Arc<ArrowOdbStore>,
}

impl ObjectStore for ArrowObjectStore {
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

    fn object_path_impl(&self, id: &str) -> napi::Result<String> {
        Ok(self.inner.object_key(id).to_string())
    }

    fn store_path(&self) -> String {
        format!("arrow://{}", self.inner.prefix)
    }

    fn write_target(&self) -> WriterTarget {
        WriterTarget::Arrow(self.inner.clone())
    }

    fn read_data_impl(&self, id: &str) -> napi::Result<ObjectStoreReader> {
        self.inner.reader(id)
    }

    fn insert_objects(
        &self,
        objects: Vec<(ObjectId, (Kind, Vec<u8>))>,
    ) -> napi::Result<()> {
        for (id, (kind, data)) in objects {
            self.inner.write_object_with_id(id, kind, &data)?;
        }
        Ok(())
    }
}

#[napi]
impl ArrowObjectStore {
    /// Create a new local filesystem or in-memory `ArrowObjectStore`.
    ///
    /// - `location`: directory path or `"memory"` / `"memory://"` for an in-memory store.
    /// - `object_hash`: `"sha1"` (default) or `"sha256"`.
    #[napi(constructor)]
    pub fn new(
        location: String,
        object_hash: Option<String>,
        options: Option<ObjectStoreOptions>,
    ) -> napi::Result<Self> {
        let kind = object_kind_from_option(object_hash.as_deref())?;
        let _ = options; // Unused for arrow store options

        let store: Arc<dyn ArrowObjectStoreTrait> = if location == "memory" || location.starts_with("memory://") {
            Arc::new(object_store::memory::InMemory::new())
        } else {
            let path = std::path::Path::new(&location);
            if !path.exists() {
                std::fs::create_dir_all(path).map_err(|e| err("create_dir_all", e))?;
            }
            let fs_store = object_store::local::LocalFileSystem::new_with_prefix(path)
                .map_err(|e| err("LocalFileSystem::new_with_prefix", e))?;
            Arc::new(fs_store)
        };

        Ok(ArrowObjectStore {
            inner: Arc::new(ArrowOdbStore::new(store, kind, None)),
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

    /// Return the key path in the Arrow store.
    #[napi]
    pub fn object_path(&self, id: String) -> napi::Result<String> {
        ObjectStore::object_path(self, id)
    }

    /// Returns the store path.
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

    /// Ingest a packfile `Buffer` into the store, adding all of its objects.
    #[napi]
    pub fn import_stream(&self, pack: Buffer) -> napi::Result<()> {
        ObjectStore::import_stream(self, pack)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arrow_object_store_memory() {
        let store = ArrowObjectStore::new("memory".to_string(), None, None).unwrap();
        let data = Buffer::from(b"hello arrow object store".to_vec());
        let id = store.write_object("blob".to_string(), data).unwrap();

        assert!(store.contains(id.clone()));
        let found = store.try_find(id.clone()).unwrap().unwrap();
        assert_eq!(found.kind, "blob");
        assert_eq!(found.data.as_ref(), b"hello arrow object store");

        let header = store.try_header(id.clone()).unwrap().unwrap();
        assert_eq!(header.kind, "blob");
        assert_eq!(header.size, 24.0);

        let prefix_res = store.lookup_prefix(id[..8].to_string()).unwrap();
        assert!(prefix_res.found);
        assert_eq!(prefix_res.id.unwrap(), id);
    }
}
