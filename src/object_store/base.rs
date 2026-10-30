//! Shared base for the lower-level object stores.
//!
//! [`LooseObjectStore`] and [`MemoryObjectStore`] both implement [`ObjectStore`], which factors out
//! their common napi surface. Each variant supplies only the storage-specific primitives via the
//! *required* trait methods; the *default* methods implement the napi-facing glue (exists / lookup /
//! read / write / stream / `import_stream`) once. The default `import_stream` decodes a packfile
//! via [`decode_pack`] and hands the objects to `insert_objects`.

use napi::bindgen_prelude::Buffer;

use gix::object::Kind;
use gix::zlib::{Decompress, Inflate};
use gix::ObjectId;

use crate::error::{err, parse_oid};
use crate::object::{kind_to_string, string_to_kind};
use crate::object_store::hash_kind_format;
use crate::object_store::{
    ObjectStoreReader, ObjectStoreWriter, PrefixLookup, WriterTarget,
};

/// Shared base implemented by [`crate::object_store::LooseObjectStore`] and
/// [`crate::object_store::MemoryObjectStore`].
///
/// It factors out the object-store napi surface (exists / lookup / read / write / stream) so each
/// variant only provides the storage-specific primitives via the `required` methods below; the
/// `default` methods implement the napi-facing glue once.
pub trait ObjectStore {
    /// The hash kind this store uses.
    fn kind(&self) -> gix::hash::Kind;

    /// Whether an object with the given hex `id` exists.
    fn contains_id(&self, id: &str) -> bool;

    /// Resolve a short hex `prefix` to a unique object.
    fn lookup_prefix_impl(&self, prefix: &str) -> napi::Result<PrefixLookup>;

    /// Read an object's body + kind, or `None` if missing.
    fn find_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectData>>;

    /// Read an object's header (kind + size), or `None` if missing.
    fn header_impl(&self, id: &str) -> napi::Result<Option<crate::object::ObjectHeader>>;

    /// Write a raw object of `kind`, returning its computed id.
    fn write_buf_impl(&self, kind: Kind, data: &[u8]) -> napi::Result<ObjectId>;

    /// Write a raw object under a known `id` (hash not recomputed).
    fn write_buf_with_id_impl(
        &self,
        kind: Kind,
        data: &[u8],
        id: ObjectId,
    ) -> napi::Result<ObjectId>;

    /// The on-disk path of the loose object file for `id` (errors for in-memory stores).
    fn object_path_impl(&self, id: &str) -> napi::Result<String>;

    /// The store's path (or `"memory"` for in-memory stores).
    fn store_path(&self) -> String;

    /// The writer target this store flushes buffered objects into.
    fn write_target(&self) -> WriterTarget;

    /// Load an object fully and return a streaming reader.
    fn read_data_impl(&self, id: &str) -> napi::Result<ObjectStoreReader>;

    /// Insert decoded `(id, (kind, data))` objects (from a pack) into the store.
    fn insert_objects(
        &self,
        objects: Vec<(ObjectId, (Kind, Vec<u8>))>,
    ) -> napi::Result<()>;

    // --- Shared napi-facing glue (default implementations) ---

    fn contains(&self, id: String) -> bool {
        self.contains_id(&id)
    }

    fn lookup_prefix(&self, prefix: String) -> napi::Result<PrefixLookup> {
        self.lookup_prefix_impl(&prefix)
    }

    fn try_find(&self, id: String) -> napi::Result<Option<crate::object::ObjectData>> {
        self.find_impl(&id)
    }

    fn try_header(&self, id: String) -> napi::Result<Option<crate::object::ObjectHeader>> {
        self.header_impl(&id)
    }

    fn object_path(&self, id: String) -> napi::Result<String> {
        self.object_path_impl(&id)
    }

    fn write_object(&self, kind: String, data: Buffer) -> napi::Result<String> {
        let kind = string_to_kind(&kind)?;
        let id = self.write_buf_impl(kind, &data)?;
        Ok(id.to_string())
    }

    fn write_object_with_id(
        &self,
        kind: String,
        data: Buffer,
        id: String,
    ) -> napi::Result<String> {
        let kind = string_to_kind(&kind)?;
        let oid = parse_oid(&id)?;
        let written = self.write_buf_with_id_impl(kind, &data, oid)?;
        Ok(written.to_string())
    }

    fn path(&self) -> String {
        self.store_path()
    }

    fn object_hash(&self) -> String {
        self.kind().to_string()
    }

    fn object_format(&self) -> String {
        hash_kind_format(self.kind()).to_string()
    }

    fn reader(&self, id: String) -> napi::Result<ObjectStoreReader> {
        self.read_data_impl(&id)
    }

    fn writer(&self, kind: String, size: f64) -> napi::Result<ObjectStoreWriter> {
        let kind = string_to_kind(&kind)?;
        Ok(ObjectStoreWriter {
            target: self.write_target(),
            kind,
            buf: Vec::with_capacity(size as usize),
            expected_size: if size <= 0.0 {
                None
            } else {
                Some(size as usize)
            },
        })
    }

    /// Ingest a packfile `Buffer` (e.g. one produced by
    /// [`crate::object_store::MemoryObjectStore::dump`]) into the store, adding all of its objects.
    /// Objects whose id already exists are overwritten.
    fn import_stream(&self, pack: Buffer) -> napi::Result<()> {
        let objects = decode_pack(&pack, self.kind())?;
        self.insert_objects(objects)
    }
}

/// Decode a packfile `Buffer` into its constituent `(id, (kind, data))` objects. Used by both the
/// memory store's `load`/`import_stream` and the loose store's `import_stream`.
pub(crate) fn decode_pack(
    pack: &[u8],
    kind: gix::hash::Kind,
) -> napi::Result<Vec<(ObjectId, (Kind, Vec<u8>))>> {
    let dir = std::env::temp_dir().join(format!("repo-pack-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).map_err(|e| err("load", e))?;
    let pack_path = dir.join("tmp.pack");
    std::fs::write(&pack_path, pack).map_err(|e| err("load", e))?;
    let file = gix_pack::data::File::at(&pack_path, kind).map_err(|e| err("load", e))?;
    let mut inflate = Inflate {
        state: Decompress::new(),
    };
    let mut cache = gix_pack::cache::Never;
    let mut offset = gix_pack::data::header::SIZE as u64;
    let mut out = Vec::new();
    while offset < file.pack_end() as u64 {
        let entry = file.entry(offset).map_err(|e| err("load", e))?;
        let header_size = entry.header_size();
        let mut data = Vec::new();
        let outcome = file
            .decode_entry(entry, &mut data, &mut inflate, &resolve_pack_base, &mut cache)
            .map_err(|e| err("load", e))?;
        let mut h = gix::hash::hasher(kind);
        let header = format!("{} {}\0", kind_to_string(outcome.kind), data.len());
        h.update(header.as_bytes());
        h.update(&data);
        let id = h.try_finalize().map_err(|e| err("load", e))?;
        out.push((id, (outcome.kind, data)));
        offset += header_size as u64 + outcome.compressed_size as u64;
    }
    Ok(out)
}

/// Resolver used when decoding a packfile without any base objects available (the in-memory store
/// only ever holds fully-resolved objects, so deltas against external bases are not supported).
fn resolve_pack_base(
    _oid: &gix::hash::oid,
    _out: &mut Vec<u8>,
) -> std::result::Result<Option<gix_pack::data::decode::entry::ResolvedBase>, gix::Error> {
    Ok(None)
}

use std::sync::Arc;

/// A cheaply cloneable, ordered collection of object store payload bytes.
#[derive(Debug, Clone)]
pub struct PutPayload(Arc<[Vec<u8>]>);

impl Default for PutPayload {
    fn default() -> Self {
        Self(Arc::new([]))
    }
}

impl PutPayload {
    /// Create a new empty [`PutPayload`]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a [`PutPayload`] from a static slice
    pub fn from_static(s: &'static [u8]) -> Self {
        s.to_vec().into()
    }

    /// Returns the total length of the payload in bytes
    pub fn content_length(&self) -> usize {
        self.0.iter().map(|b| b.len()).sum()
    }

    /// Returns an iterator over byte slices in this payload
    pub fn iter(&self) -> PutPayloadIter<'_> {
        PutPayloadIter(self.0.iter())
    }
}

impl AsRef<[Vec<u8>]> for PutPayload {
    fn as_ref(&self) -> &[Vec<u8>] {
        self.0.as_ref()
    }
}

impl<'a> IntoIterator for &'a PutPayload {
    type Item = &'a Vec<u8>;
    type IntoIter = PutPayloadIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// An iterator over [`PutPayload`]
#[derive(Debug)]
pub struct PutPayloadIter<'a>(std::slice::Iter<'a, Vec<u8>>);

impl<'a> Iterator for PutPayloadIter<'a> {
    type Item = &'a Vec<u8>;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl From<Vec<u8>> for PutPayload {
    fn from(value: Vec<u8>) -> Self {
        Self(Arc::new([value]))
    }
}

impl From<&'static str> for PutPayload {
    fn from(value: &'static str) -> Self {
        value.as_bytes().to_vec().into()
    }
}

impl From<&'static [u8]> for PutPayload {
    fn from(value: &'static [u8]) -> Self {
        value.to_vec().into()
    }
}

impl From<String> for PutPayload {
    fn from(value: String) -> Self {
        value.into_bytes().into()
    }
}

impl From<PutPayload> for Vec<u8> {
    fn from(value: PutPayload) -> Self {
        match value.0.len() {
            0 => Vec::new(),
            1 => value.0[0].clone(),
            _ => {
                let mut buf = Vec::with_capacity(value.content_length());
                value.iter().for_each(|x| buf.extend_from_slice(x));
                buf
            }
        }
    }
}

/// A builder for [`PutPayload`] that avoids reallocating memory.
#[derive(Debug)]
pub struct PutPayloadMut {
    len: usize,
    completed: Vec<Vec<u8>>,
    in_progress: Vec<u8>,
    block_size: usize,
}

impl Default for PutPayloadMut {
    fn default() -> Self {
        Self {
            len: 0,
            completed: vec![],
            in_progress: vec![],
            block_size: 8 * 1024,
        }
    }
}

impl PutPayloadMut {
    /// Create a new [`PutPayloadMut`]
    pub fn new() -> Self {
        Self::default()
    }

    /// Configures the minimum allocation size
    pub fn with_block_size(self, block_size: usize) -> Self {
        Self { block_size, ..self }
    }

    /// Write bytes into this [`PutPayloadMut`]
    pub fn extend_from_slice(&mut self, slice: &[u8]) {
        let remaining = self.in_progress.capacity() - self.in_progress.len();
        let to_copy = remaining.min(slice.len());

        self.in_progress.extend_from_slice(&slice[..to_copy]);
        if self.in_progress.capacity() == self.in_progress.len() {
            let new_cap = self.block_size.max(slice.len() - to_copy);
            let completed = std::mem::replace(&mut self.in_progress, Vec::with_capacity(new_cap));
            if !completed.is_empty() {
                self.completed.push(completed)
            }
            self.in_progress.extend_from_slice(&slice[to_copy..])
        }
        self.len += slice.len();
    }

    /// Append byte vector to this [`PutPayloadMut`] without copying
    pub fn push(&mut self, bytes: Vec<u8>) {
        if !self.in_progress.is_empty() {
            let completed = std::mem::take(&mut self.in_progress);
            self.completed.push(completed)
        }
        self.len += bytes.len();
        self.completed.push(bytes);
    }

    /// Returns `true` if this [`PutPayloadMut`] contains no bytes
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the total length in bytes
    #[inline]
    pub fn content_length(&self) -> usize {
        self.len
    }

    /// Convert into [`PutPayload`]
    pub fn freeze(mut self) -> PutPayload {
        if !self.in_progress.is_empty() {
            let completed = std::mem::take(&mut self.in_progress);
            self.completed.push(completed);
        }
        PutPayload(self.completed.into())
    }
}

impl From<PutPayloadMut> for PutPayload {
    fn from(value: PutPayloadMut) -> Self {
        value.freeze()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_put_payload_and_mut() {
        let mut builder = PutPayloadMut::new().with_block_size(16);
        builder.extend_from_slice(b"hello ");
        builder.extend_from_slice(b"world!");
        assert_eq!(builder.content_length(), 12);
        assert!(!builder.is_empty());

        builder.push(b" zero copy".to_vec());
        assert_eq!(builder.content_length(), 22);

        let payload: PutPayload = builder.freeze();
        assert_eq!(payload.content_length(), 22);

        let vec: Vec<u8> = payload.into();
        assert_eq!(vec, b"hello world! zero copy");
    }
}
