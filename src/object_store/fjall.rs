//! Fjall-backed embedded key-value object store.

use std::path::PathBuf;
use std::sync::Arc;

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use fjall::{Config, Database, Keyspace, KeyspaceCreateOptions};
use gix::ObjectId;
use gix::object::Kind;

use crate::error::{err, err_str};
use crate::object::kind_to_string;
use crate::object_store::base::ObjectStore;
use crate::object_store::{
    ObjectStoreReader, ObjectStoreWriter, PrefixLookup, WriterTarget, object_kind_from_option,
};

/// Embedded LSM-tree object store backed by Fjall.
#[napi]
pub struct FjallObjectStore {
    _db: Arc<Database>,
    keyspace: Keyspace,
    kind: gix::hash::Kind,
    path: String,
}

impl FjallObjectStore {
    fn encode_value(kind: Kind, data: &[u8]) -> Vec<u8> {
        let kind_byte = match kind {
            Kind::Commit => 1u8,
            Kind::Tree => 2u8,
            Kind::Blob => 3u8,
            Kind::Tag => 4u8,
        };
        let mut val = Vec::with_capacity(1 + data.len());
        val.push(kind_byte);
        val.extend_from_slice(data);
        val
    }

    fn decode_value(val: &[u8]) -> Option<(Kind, Vec<u8>)> {
        if val.is_empty() {
            return None;
        }
        let kind = match val[0] {
            1 => Kind::Commit,
            2 => Kind::Tree,
            3 => Kind::Blob,
            4 => Kind::Tag,
            _ => return None,
        };
        Some((kind, val[1..].to_vec()))
    }
}

impl ObjectStore for FjallObjectStore {
    fn kind(&self) -> gix::hash::Kind {
        self.kind
    }

    fn contains_id(&self, id: &str) -> bool {
        self.keyspace.contains_key(id.as_bytes()).unwrap_or(false)
    }

    fn lookup_prefix_impl(&self, prefix: &str) -> napi::Result<PrefixLookup> {
        let p = prefix.to_ascii_lowercase();
        let mut found: Option<String> = None;
        let mut count = 0usize;

        for item in self.keyspace.prefix(p.as_bytes()) {
            if let Ok(key_bytes) = item.key() {
                if let Ok(key_str) = std::str::from_utf8(&key_bytes) {
                    if key_str.starts_with(&p) {
                        count += 1;
                        found = Some(key_str.to_string());
                    }
                }
            }
        }

        Ok(match found {
            Some(id) if count == 1 => PrefixLookup {
                found: true,
                ambiguous: false,
                id: Some(id),
            },
            Some(_) => PrefixLookup {
                found: false,
                ambiguous: true,
                id: None,
            },
            None => PrefixLookup {
                found: false,
                ambiguous: false,
                id: None,
            },
        })
    }

    fn find_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectData>> {
        let value = self.keyspace.get(id.as_bytes()).map_err(|e| err("fjall.get", e))?;
        let Some(value) = value else {
            return Ok(None);
        };
        let Some((kind, data)) = Self::decode_value(&value) else {
            return Ok(None);
        };
        Ok(Some(crate::object::ObjectData {
            kind: kind_to_string(kind),
            data: Buffer::from(data),
        }))
    }

    fn header_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectHeader>> {
        let value = self.keyspace.get(id.as_bytes()).map_err(|e| err("fjall.get", e))?;
        let Some(value) = value else {
            return Ok(None);
        };
        let Some((kind, data)) = Self::decode_value(&value) else {
            return Ok(None);
        };
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
        let id = h.try_finalize().map_err(|e| err("fjall.write", e))?;

        let val = Self::encode_value(kind, data);
        self.keyspace
            .insert(id.to_string().as_bytes(), val)
            .map_err(|e| err("fjall.insert", e))?;
        Ok(id)
    }

    fn write_buf_with_id_impl(
        &self,
        kind: Kind,
        data: &[u8],
        id: ObjectId,
    ) -> napi::Result<ObjectId> {
        let val = Self::encode_value(kind, data);
        self.keyspace
            .insert(id.to_string().as_bytes(), val)
            .map_err(|e| err("fjall.insert", e))?;
        Ok(id)
    }

    fn object_path_impl(&self, _id: &str) -> napi::Result<String> {
        Err(err_str(
            "object_path",
            "fjall object store entries have no individual on-disk file path",
        ))
    }

    fn store_path(&self) -> String {
        self.path.clone()
    }

    fn write_target(&self) -> WriterTarget {
        WriterTarget::Memory(Arc::new(crate::object_store::memory::InMemoryStore::new(
            self.kind,
        )))
    }

    fn read_data_impl(&self, id: &str) -> napi::Result<ObjectStoreReader> {
        let value = self.keyspace.get(id.as_bytes()).map_err(|e| err("fjall.get", e))?;
        let Some(value) = value else {
            return Err(err_str("reader", format!("object {id} not found")));
        };
        let Some((kind, data)) = Self::decode_value(&value) else {
            return Err(err_str("reader", format!("corrupt object data for {id}")));
        };
        Ok(ObjectStoreReader {
            data,
            kind: kind_to_string(kind),
            pos: 0,
        })
    }

    fn insert_objects(&self, objects: Vec<(ObjectId, (Kind, Vec<u8>))>) -> napi::Result<()> {
        for (id, (kind, data)) in objects {
            let val = Self::encode_value(kind, &data);
            self.keyspace
                .insert(id.to_string().as_bytes(), val)
                .map_err(|e| err("fjall.insert_batch", e))?;
        }
        Ok(())
    }
}

#[napi]
impl FjallObjectStore {
    /// Open or create a Fjall database at `path`.
    #[napi(constructor)]
    pub fn new(path: String, object_hash: Option<String>) -> napi::Result<Self> {
        let kind = object_kind_from_option(object_hash.as_deref())?;
        let db_path = PathBuf::from(&path);
        let db = Database::open(Config::new(&db_path)).map_err(|e| err("fjall.open", e))?;
        let keyspace = db
            .keyspace("objects", KeyspaceCreateOptions::default)
            .map_err(|e| err("fjall.keyspace", e))?;

        Ok(Self {
            _db: Arc::new(db),
            keyspace,
            kind,
            path,
        })
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
