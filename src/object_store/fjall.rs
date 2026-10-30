//! An [`ObjectStore`](crate::object_store::base::ObjectStore) implementation backed by [`fjall`], an
//! embedded LSM-tree key-value storage engine.

use std::sync::Arc;

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use gix::object::Kind;
use gix::ObjectId;

use crate::error::{err, err_str};
use crate::object::{kind_to_string, string_to_kind};
use crate::object_store::base::ObjectStore;
use crate::object_store::{
    object_kind_from_option, parse_hash_or_options, ObjectStoreOptions, ObjectStoreReader,
    ObjectStoreWriter, PrefixLookup, WriterTarget,
};

fn encode_git_object(kind: Kind, data: &[u8]) -> Vec<u8> {
    let header = format!("{} {}\0", kind_to_string(kind), data.len());
    let mut out = Vec::with_capacity(header.len() + data.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(data);
    out
}

fn decode_git_object(raw: &[u8]) -> napi::Result<(Kind, Vec<u8>)> {
    let nul_pos = raw.iter().position(|&b| b == 0).ok_or_else(|| {
        err_str("decode_object", "invalid git object: missing header terminator")
    })?;
    let header_str = std::str::from_utf8(&raw[..nul_pos])
        .map_err(|e| err_str("decode_object", format!("invalid header utf8: {e}")))?;
    let mut parts = header_str.split_whitespace();
    let kind_str = parts
        .next()
        .ok_or_else(|| err_str("decode_object", "missing object kind"))?;
    let _size_str = parts
        .next()
        .ok_or_else(|| err_str("decode_object", "missing object size"))?;
    let kind = string_to_kind(kind_str)?;
    let data = raw[nul_pos + 1..].to_vec();
    Ok((kind, data))
}

/// An object database store backed by [`fjall`], an embedded LSM key-value storage engine.
#[napi]
pub struct FjallObjectStore {
    _db: Arc<fjall::Database>,
    keyspace: fjall::Keyspace,
    kind: gix::hash::Kind,
    path: String,
}

impl ObjectStore for FjallObjectStore {
    fn kind(&self) -> gix::hash::Kind {
        self.kind
    }

    fn contains_id(&self, id: &str) -> bool {
        self.keyspace
            .contains_key(id.as_bytes())
            .unwrap_or(false)
    }

    fn lookup_prefix_impl(&self, prefix: &str) -> napi::Result<PrefixLookup> {
        let p = prefix.to_ascii_lowercase();
        let mut count = 0usize;
        let mut found_id = None;

        for guard in self.keyspace.prefix(p.as_bytes()) {
            if let Ok(k) = guard.key() {
                if let Ok(key_str) = std::str::from_utf8(&k) {
                    if key_str.starts_with(&p) {
                        count += 1;
                        found_id = Some(key_str.to_string());
                    }
                }
            }
        }

        if count == 1 {
            Ok(PrefixLookup {
                found: true,
                ambiguous: false,
                id: found_id,
            })
        } else if count > 1 {
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
        let raw = match self.keyspace.get(id.as_bytes()).map_err(|e| err("find_impl", e))? {
            Some(data) => data,
            None => return Ok(None),
        };

        let (kind, data) = decode_git_object(&raw)?;
        Ok(Some(crate::object::ObjectData {
            kind: kind_to_string(kind),
            data: Buffer::from(data),
        }))
    }

    fn header_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectHeader>> {
        let raw = match self.keyspace.get(id.as_bytes()).map_err(|e| err("header_impl", e))? {
            Some(data) => data,
            None => return Ok(None),
        };

        let (kind, data) = decode_git_object(&raw)?;
        Ok(Some(crate::object::ObjectHeader {
            kind: kind_to_string(kind),
            size: data.len() as f64,
            is_packed: false,
            num_deltas: None,
        }))
    }

    fn write_buf_impl(&self, kind: Kind, data: &[u8]) -> napi::Result<ObjectId> {
        let mut h = gix::hash::hasher(self.kind);
        let header = format!("{} {}\0", kind_to_string(kind), data.len());
        h.update(header.as_bytes());
        h.update(data);
        let id = h.try_finalize().map_err(|e| err("write_object", e))?;

        let id_str = id.to_string();
        let raw = encode_git_object(kind, data);
        self.keyspace
            .insert(id_str.as_bytes(), raw)
            .map_err(|e| err("write_object", e))?;

        Ok(id)
    }

    fn write_buf_with_id_impl(
        &self,
        kind: Kind,
        data: &[u8],
        id: ObjectId,
    ) -> napi::Result<ObjectId> {
        let id_str = id.to_string();
        let raw = encode_git_object(kind, data);
        self.keyspace
            .insert(id_str.as_bytes(), raw)
            .map_err(|e| err("write_object_with_id", e))?;

        Ok(id)
    }

    fn object_path_impl(&self, _id: &str) -> napi::Result<String> {
        Err(err_str(
            "object_path",
            "fjall object store does not expose individual object file paths",
        ))
    }

    fn store_path(&self) -> String {
        self.path.clone()
    }

    fn write_target(&self) -> WriterTarget {
        let keyspace = self.keyspace.clone();
        let hash_kind = self.kind;
        WriterTarget::Custom(Arc::new(move |obj_kind, data| {
            let mut h = gix::hash::hasher(hash_kind);
            let header = format!("{} {}\0", kind_to_string(obj_kind), data.len());
            h.update(header.as_bytes());
            h.update(data);
            let id = h.try_finalize().map_err(|e| err("write_object", e))?;

            let id_str = id.to_string();
            let raw = encode_git_object(obj_kind, data);
            keyspace
                .insert(id_str.as_bytes(), raw)
                .map_err(|e| err("write_object", e))?;

            Ok(id)
        }))
    }

    fn read_data_impl(&self, id: &str) -> napi::Result<ObjectStoreReader> {
        let raw = match self.keyspace.get(id.as_bytes()).map_err(|e| err("reader", e))? {
            Some(data) => data,
            None => return Err(err_str("reader", format!("object {id} not found"))),
        };

        let (kind, data) = decode_git_object(&raw)?;
        Ok(ObjectStoreReader {
            data,
            kind: kind_to_string(kind),
            pos: 0,
            max_chunk_size: None,
        })
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
impl FjallObjectStore {
    /// Open or create a Fjall key-value object store at `directory`.
    #[napi(constructor)]
    pub fn new(
        directory: String,
        object_hash: Option<napi::bindgen_prelude::Either<String, ObjectStoreOptions>>,
    ) -> napi::Result<Self> {
        let (hash_str, _) = parse_hash_or_options(object_hash);
        let kind = object_kind_from_option(hash_str.as_deref())?;
        std::fs::create_dir_all(&directory).map_err(|e| err("FjallObjectStore::new", e))?;

        let db = fjall::Database::builder(&directory)
            .open()
            .map_err(|e| err("FjallObjectStore::new", e))?;
        let db = Arc::new(db);
        let keyspace = db
            .keyspace("objects", fjall::KeyspaceCreateOptions::default)
            .map_err(|e| err("FjallObjectStore::new", e))?;

        Ok(Self {
            _db: db,
            keyspace,
            kind,
            path: directory,
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

    /// Return the database directory path.
    #[napi(getter)]
    pub fn path(&self) -> String {
        ObjectStore::path(self)
    }

    /// Fjall store does not expose individual file paths; this errors.
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

    /// Return the hex ids of every object stored in Fjall.
    #[napi]
    pub fn entries(&self) -> napi::Result<Vec<String>> {
        let mut out = Vec::new();
        for guard in self.keyspace.iter() {
            if let Ok(k) = guard.key() {
                if let Ok(key_str) = std::str::from_utf8(&k) {
                    out.push(key_str.to_string());
                }
            }
        }
        Ok(out)
    }

    /// Return the total number of objects stored in Fjall.
    #[napi]
    pub fn size(&self) -> napi::Result<f64> {
        let count = self.keyspace.len().map_err(|e| err("size", e))?;
        Ok(count as f64)
    }
}
