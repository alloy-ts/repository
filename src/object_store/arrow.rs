//! An adapter bridging Apache Arrow's [`object_store::ObjectStore`] to the git ODB [`ObjectStore`](crate::object_store::base::ObjectStore) interface.
//!
//! This enables any Arrow-compatible object store (e.g. `object_store::local::LocalFileSystem`,
//! S3, Azure, GCP, or in-memory) to be used as a backend for storing and retrieving loose git objects.

use std::io::{Read, Write};
use std::sync::Arc;

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;
use object_store::path::Path as ArrowPath;
use object_store::ObjectStore as ArrowStore;
use object_store::ObjectStoreExt;

use gix::object::Kind;
use gix::zlib::stream::deflate;
use flate2::read::ZlibDecoder;
use gix::zlib::Compression;
use gix::ObjectId;

use crate::error::{err, err_str};
use crate::object::{kind_to_string, string_to_kind};
use crate::object_store::base::ObjectStore;
use crate::object_store::{
    object_kind_from_option, ObjectStoreReader, ObjectStoreWriter, PrefixLookup, WriterTarget,
};

/// Metadata describing an object stored in an Arrow object store.
#[napi(object)]
pub struct ArrowObjectMeta {
    /// Object path/location.
    pub location: String,
    /// Last modified ISO timestamp string.
    pub last_modified: String,
    /// Object size in bytes.
    pub size: f64,
    /// Entity tag if available.
    pub e_tag: Option<String>,
    /// Object version if available.
    pub version: Option<String>,
}

impl From<object_store::ObjectMeta> for ArrowObjectMeta {
    fn from(meta: object_store::ObjectMeta) -> Self {
        Self {
            location: meta.location.to_string(),
            last_modified: meta.last_modified.to_rfc3339(),
            size: meta.size as f64,
            e_tag: meta.e_tag,
            version: meta.version,
        }
    }
}

/// Result of listing objects with a delimiter.
#[napi(object)]
pub struct ArrowListResult {
    /// Common prefixes (directories).
    pub common_prefixes: Vec<String>,
    /// Objects in the current directory level.
    pub objects: Vec<ArrowObjectMeta>,
}

/// An adapter that wraps an Arrow [`object_store::ObjectStore`] and implements
/// git ODB [`ObjectStore`] operations over it.
#[napi]
pub struct ArrowObjectStore {
    inner: Arc<dyn ArrowStore>,
    kind: gix::hash::Kind,
    prefix: String,
}

impl ArrowObjectStore {
    /// Create a new [`ArrowObjectStore`] wrapping any raw `Arc<dyn ArrowStore>`.
    pub fn new_raw(
        inner: Arc<dyn ArrowStore>,
        kind: gix::hash::Kind,
        prefix: Option<String>,
    ) -> Self {
        Self {
            inner,
            kind,
            prefix: prefix.unwrap_or_default(),
        }
    }

    fn object_key(&self, id: &str) -> ArrowPath {
        if id.len() >= 2 {
            let (dir, file) = id.split_at(2);
            if self.prefix.is_empty() {
                ArrowPath::from(format!("{dir}/{file}"))
            } else {
                ArrowPath::from(format!("{}/{dir}/{file}", self.prefix))
            }
        } else if self.prefix.is_empty() {
            ArrowPath::from(id)
        } else {
            ArrowPath::from(format!("{}/{}", self.prefix, id))
        }
    }
}

impl ObjectStore for ArrowObjectStore {
    fn kind(&self) -> gix::hash::Kind {
        self.kind
    }

    fn contains_id(&self, id: &str) -> bool {
        let key = self.object_key(id);
        futures::executor::block_on(async {
            self.inner.head(&key).await.is_ok()
        })
    }

    fn lookup_prefix_impl(&self, prefix: &str) -> napi::Result<PrefixLookup> {
        let p = prefix.to_ascii_lowercase();
        let list_prefix = if p.len() >= 2 {
            let (dir, _) = p.split_at(2);
            if self.prefix.is_empty() {
                Some(ArrowPath::from(dir))
            } else {
                Some(ArrowPath::from(format!("{}/{dir}", self.prefix)))
            }
        } else if !self.prefix.is_empty() {
            Some(ArrowPath::from(self.prefix.as_str()))
        } else {
            None
        };

        let mut matches = Vec::new();
        let inner = Arc::clone(&self.inner);

        futures::executor::block_on(async {
            use futures::StreamExt;
            let mut stream = inner.list(list_prefix.as_ref());
            while let Some(res) = stream.next().await {
                if let Ok(meta) = res {
                    let path_str = meta.location.to_string();
                    let hex = path_str
                        .split('/')
                        .rev()
                        .take(2)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<String>();
                    if hex.starts_with(&p) {
                        matches.push(hex);
                    }
                }
            }
        });

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

    fn find_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectData>> {
        let key = self.object_key(id);
        let inner = Arc::clone(&self.inner);
        let result = futures::executor::block_on(async {
            inner.get(&key).await
        });

        match result {
            Ok(get_res) => {
                let bytes = futures::executor::block_on(async { get_res.bytes().await })
                    .map_err(|e| err("find", e))?;

                let mut decompressed = Vec::new();
                let mut decoder = ZlibDecoder::new(bytes.as_ref());
                decoder.read_to_end(&mut decompressed).map_err(|e| err("decompress", e))?;

                let null_pos = decompressed.iter().position(|&b| b == 0)
                    .ok_or_else(|| err_str("find", "invalid loose object header"))?;

                let header_str = std::str::from_utf8(&decompressed[..null_pos])
                    .map_err(|e| err("header", e))?;

                let mut parts = header_str.split(' ');
                let kind_str = parts.next().unwrap_or("blob");
                let content = decompressed[null_pos + 1..].to_vec();

                Ok(Some(crate::object::ObjectData {
                    kind: kind_str.to_string(),
                    data: Buffer::from(content),
                }))
            }
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(err("find", e)),
        }
    }

    fn header_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectHeader>> {
        if let Some(data) = self.find_impl(id)? {
            Ok(Some(crate::object::ObjectHeader {
                kind: data.kind,
                size: data.data.len() as f64,
                is_packed: false,
                num_deltas: None,
            }))
        } else {
            Ok(None)
        }
    }

    fn write_buf_impl(&self, kind: Kind, data: &[u8]) -> napi::Result<ObjectId> {
        let mut h = gix::hash::hasher(self.kind);
        let header = format!("{} {}\0", kind_to_string(kind), data.len());
        h.update(header.as_bytes());
        h.update(data);
        let id = h.try_finalize().map_err(|e| err("write_object", e))?;

        self.write_buf_with_id_impl(kind, data, id)
    }

    fn write_buf_with_id_impl(
        &self,
        kind: Kind,
        data: &[u8],
        id: ObjectId,
    ) -> napi::Result<ObjectId> {
        let key = self.object_key(&id.to_string());
        let header = format!("{} {}\0", kind_to_string(kind), data.len());

        let mut payload_bytes = Vec::new();
        payload_bytes.extend_from_slice(header.as_bytes());
        payload_bytes.extend_from_slice(data);

        let mut enc = deflate::Write::new(Vec::new(), Compression::default());
        enc.write_all(&payload_bytes).map_err(|e| err("compress", e))?;
        enc.flush().map_err(|e| err("compress", e))?;
        let compressed = enc.into_inner();

        let inner = Arc::clone(&self.inner);
        futures::executor::block_on(async {
            inner.put(&key, compressed.into()).await
        }).map_err(|e| err("write_object", e))?;

        Ok(id)
    }

    fn object_path_impl(&self, id: &str) -> napi::Result<String> {
        Ok(self.object_key(id).to_string())
    }

    fn store_path(&self) -> String {
        format!("arrow:{}", self.inner)
    }

    fn write_target(&self) -> WriterTarget {
        WriterTarget::Memory(Arc::new(crate::object_store::memory::InMemoryStore::new(self.kind)))
    }

    fn read_data_impl(&self, id: &str) -> napi::Result<ObjectStoreReader> {
        if let Some(data) = self.find_impl(id)? {
            Ok(ObjectStoreReader {
                data: data.data.to_vec(),
                kind: data.kind,
                pos: 0,
            })
        } else {
            Err(err_str("reader", format!("object {id} not found")))
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
    /// Create a new ArrowObjectStore backed by local filesystem directory.
    #[napi(constructor)]
    pub fn new(
        directory: String,
        object_hash: Option<String>,
        prefix: Option<String>,
    ) -> napi::Result<Self> {
        let kind = object_kind_from_option(object_hash.as_deref())?;
        let local_store = object_store::local::LocalFileSystem::new_with_prefix(&directory)
            .map_err(|e| err("ArrowObjectStore::new", e))?;
        Ok(Self {
            inner: Arc::new(local_store),
            kind,
            prefix: prefix.unwrap_or_default(),
        })
    }

    /// Create an in-memory ArrowObjectStore.
    #[napi]
    pub fn from_memory(
        object_hash: Option<String>,
        prefix: Option<String>,
    ) -> napi::Result<Self> {
        let kind = object_kind_from_option(object_hash.as_deref())?;
        let mem_store = object_store::memory::InMemory::new();
        Ok(Self {
            inner: Arc::new(mem_store),
            kind,
            prefix: prefix.unwrap_or_default(),
        })
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

    /// Read an object's raw data and kind.
    #[napi]
    pub fn try_find(&self, id: String) -> napi::Result<Option<crate::object::ObjectData>> {
        ObjectStore::try_find(self, id)
    }

    /// Read an object's header (kind + size).
    #[napi]
    pub fn try_header(&self, id: String) -> napi::Result<Option<crate::object::ObjectHeader>> {
        ObjectStore::try_header(self, id)
    }

    /// Write a raw object of `kind` (`commit`, `tree`, `blob`, `tag`) and return its id.
    #[napi]
    pub fn write_object(&self, kind: String, data: Buffer) -> napi::Result<String> {
        ObjectStore::write_object(self, kind, data)
    }

    /// Write a raw object under `id` without recomputing hash.
    #[napi]
    pub fn write_object_with_id(
        &self,
        kind: String,
        data: Buffer,
        id: String,
    ) -> napi::Result<String> {
        ObjectStore::write_object_with_id(self, kind, data, id)
    }

    /// Return object key path.
    #[napi]
    pub fn object_path(&self, id: String) -> napi::Result<String> {
        ObjectStore::object_path(self, id)
    }

    /// Store path representation.
    #[napi(getter)]
    pub fn path(&self) -> String {
        ObjectStore::path(self)
    }

    /// Hash algorithm used (`"sha1"` or `"sha256"`).
    #[napi(getter)]
    pub fn object_hash(&self) -> String {
        ObjectStore::object_hash(self)
    }

    /// Lower-case object format.
    #[napi(getter)]
    pub fn object_format(&self) -> String {
        ObjectStore::object_format(self)
    }

    /// Open streaming reader.
    #[napi]
    pub fn reader(&self, id: String) -> napi::Result<ObjectStoreReader> {
        ObjectStore::reader(self, id)
    }

    /// Open streaming writer.
    #[napi]
    pub fn writer(&self, kind: String, size: f64) -> napi::Result<ObjectStoreWriter> {
        ObjectStore::writer(self, kind, size)
    }

    /// Ingest a packfile Buffer into the store.
    #[napi]
    pub fn import_stream(&self, pack: Buffer) -> napi::Result<()> {
        ObjectStore::import_stream(self, pack)
    }

    /// List all objects with optional prefix.
    #[napi]
    pub fn list(&self, prefix: Option<String>) -> napi::Result<Vec<ArrowObjectMeta>> {
        let path_prefix = prefix.as_deref().map(ArrowPath::from);
        let mut results = Vec::new();
        let inner = Arc::clone(&self.inner);
        futures::executor::block_on(async {
            use futures::StreamExt;
            let mut stream = inner.list(path_prefix.as_ref());
            while let Some(res) = stream.next().await {
                if let Ok(meta) = res {
                    results.push(ArrowObjectMeta::from(meta));
                }
            }
        });
        Ok(results)
    }

    /// List objects with prefix and delimiter.
    #[napi]
    pub fn list_with_delimiter(&self, prefix: Option<String>) -> napi::Result<ArrowListResult> {
        let path_prefix = prefix.as_deref().map(ArrowPath::from);
        let inner = Arc::clone(&self.inner);
        let res = futures::executor::block_on(async {
            inner.list_with_delimiter(path_prefix.as_ref()).await
        }).map_err(|e| err("list_with_delimiter", e))?;

        Ok(ArrowListResult {
            common_prefixes: res.common_prefixes.into_iter().map(|p| p.to_string()).collect(),
            objects: res.objects.into_iter().map(ArrowObjectMeta::from).collect(),
        })
    }

    /// List objects with prefix and location greater than offset.
    #[napi]
    pub fn list_with_offset(
        &self,
        prefix: Option<String>,
        offset: String,
    ) -> napi::Result<Vec<ArrowObjectMeta>> {
        let path_prefix = prefix.as_deref().map(ArrowPath::from);
        let offset_path = ArrowPath::from(offset.as_str());
        let mut results = Vec::new();
        let inner = Arc::clone(&self.inner);
        futures::executor::block_on(async {
            use futures::StreamExt;
            let mut stream = inner.list_with_offset(path_prefix.as_ref(), &offset_path);
            while let Some(res) = stream.next().await {
                if let Ok(meta) = res {
                    results.push(ArrowObjectMeta::from(meta));
                }
            }
        });
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::memory::InMemory;

    #[test]
    fn test_arrow_object_store_roundtrip() {
        let memory_store = Arc::new(InMemory::new());
        let git_store = ArrowObjectStore::new_raw(memory_store, gix::hash::Kind::Sha1, None);

        let content = b"hello arrow git object store\n";
        let id = git_store.write_buf_impl(Kind::Blob, content).unwrap();

        assert!(git_store.contains_id(&id.to_string()));

        let found = git_store.find_impl(&id.to_string()).unwrap().unwrap();
        assert_eq!(found.kind, "blob");
        assert_eq!(found.data.as_ref(), content);
    }
}
