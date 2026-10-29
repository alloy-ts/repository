use std::io::SeekFrom;

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use gix::ObjectId;
use gix::object::Kind;

use crate::error::{err, err_str};
use crate::object_store::base::GenericObjectStore;
use crate::object_store::base::ObjectStore;
use crate::object_store::{ObjectStoreReader, ObjectStoreWriter, PrefixLookup, WriterTarget};

#[napi]
pub struct BufferedObjectStore {
    inner: GenericObjectStore,
}

impl ObjectStore for BufferedObjectStore {
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
        self.inner.store_path()
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
impl BufferedObjectStore {
    /// Open the persistent object database rooted at `objects_directory` (created if absent).
    /// `object_hash` is `"sha1"` (default) or `"sha256"`.
    #[napi(constructor)]
    pub fn new(objects_directory: String, object_hash: Option<String>) -> napi::Result<Self> {
        let inner = GenericObjectStore::new(objects_directory, object_hash)?;
        Ok(BufferedObjectStore { inner })
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

    /// A prolly-backed store has no loose-object file path, so this always errors.
    #[napi]
    pub fn object_path(&self, _id: String) -> napi::Result<String> {
        ObjectStore::object_path(self, _id)
    }

    /// The node-store directory this store is rooted at.
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
    /// Objects whose id already exists are overwritten.
    #[napi]
    pub fn import_stream(&self, pack: Buffer) -> napi::Result<()> {
        ObjectStore::import_stream(self, pack)
    }

    /// Serialize every object currently held in the store into a git packfile `Buffer`.
    #[napi]
    pub fn dump(&self) -> napi::Result<Buffer> {
        self.inner.dump()
    }

    /// Load a packfile `Buffer` (e.g. one produced by [`dump`](Self::dump)) into the store,
    /// adding all of its objects. Objects whose id already exists are overwritten.
    #[napi]
    pub fn load(&self, pack: Buffer) -> napi::Result<()> {
        self.inner.load(pack)
    }

    /// Return the hex ids of every object, in insertion order (hash order after a
    /// restore/load, where insertion order is unrecoverable).
    #[napi]
    pub fn entries(&self) -> napi::Result<Vec<String>> {
        self.inner.entries()
    }

    /// Return the number of objects currently held.
    #[napi]
    pub fn size(&self) -> napi::Result<f64> {
        self.inner.size()
    }

    /// Return the last-modified time (milliseconds since the Unix epoch) of the object
    /// identified by `id`, or `null` if it is not present.
    #[napi]
    pub fn last_modified(&self, id: String) -> napi::Result<Option<f64>> {
        self.inner.last_modified(id)
    }

    /// Copy the object identified by `from_id` to a new object stored under `to_id`,
    /// returning `to_id`.
    #[napi]
    pub fn copy(&self, from_id: String, to_id: String) -> napi::Result<String> {
        self.inner.copy(from_id, to_id)
    }

    /// Remove every object currently held in the store, leaving it empty.
    /// The persisted root (if any) is also dropped.
    #[napi]
    pub fn reset(&self) -> napi::Result<()> {
        self.inner.reset()
    }

    /// Return a new, independent store that shares the underlying node store with this one.
    /// Writes to either side derive independent roots (copy-on-write).
    #[napi]
    pub fn fork(&self) -> BufferedObjectStore {
        BufferedObjectStore {
            inner: self.inner.fork(),
        }
    }

    /// Persist the current tree root under the name `"head"` so a later
    /// [`BufferedObjectStore::new`] at the same directory reloads it.
    #[napi]
    pub fn save_root(&self) -> napi::Result<()> {
        self.inner.save_root()
    }

    /// Reload the root previously persisted via [`save_root`](Self::save_root),
    /// replacing the current contents. Errors if no root has been saved.
    #[napi]
    pub fn restore_root(&self) -> napi::Result<()> {
        self.inner.restore_root()
    }
}

/// Where an [`ObjectStoreBufReader::seek`] is relative to.
#[napi(string_enum)]
pub enum SeekWhence {
    /// Relative to the start of the object.
    Start,
    /// Relative to the current cursor.
    Current,
    /// Relative to the end of the object.
    End,
}

/// Options for constructing an [`ObjectStoreBufReader`].
#[napi(object)]
pub struct BufReaderOptions {
    /// Internal read-ahead buffer size in bytes. Currently informational only — the whole object
    /// is loaded once up front (prolly is a local content-addressed store, not a remote one).
    pub capacity: Option<u32>,
}

/// Options for constructing an [`ObjectStoreBufWriter`].
#[napi(object)]
pub struct BufWriterOptions {
    /// Bytes buffered before a flush is eligible (informational; the whole buffer is written on
    /// `shutdown`). `null` uses a 10 MiB default.
    pub capacity: Option<u32>,
    /// Max in-flight writes (informational for the local single-put path). `null` = 8.
    pub max_concurrency: Option<u32>,
}

/// A seekable buffered reader over a single object in a [`BufferedObjectStore`].
///
/// The object is loaded once at construction; [`read`](Self::read) and
/// [`seek`](Self::seek) then serve ranges from the in-memory bytes. Unlike
/// [`ObjectStoreReader`](crate::object_store::base::ObjectStoreReader), this supports seeking.
#[napi]
pub struct ObjectStoreBufReader {
    data: Vec<u8>,
    kind: String,
    pos: u64,
    size: u64,
}

#[napi]
impl ObjectStoreBufReader {
    /// Open a buffered reader for `id`. The object is loaded (and its size resolved) immediately;
    /// errors if the object is missing.
    #[napi(constructor)]
    pub fn new(
        store: &BufferedObjectStore,
        id: String,
        _options: Option<BufReaderOptions>,
    ) -> napi::Result<Self> {
        let found = ObjectStore::try_find(store, id.clone())?;
        match found {
            Some(d) => {
                let data = d.data.to_vec();
                let size = data.len() as u64;
                Ok(Self {
                    data,
                    kind: d.kind,
                    pos: 0,
                    size,
                })
            }
            None => Err(err_str("buf_reader", format!("object {id} not found"))),
        }
    }

    /// Total size of the object in bytes.
    #[napi(getter)]
    pub fn size(&self) -> f64 {
        self.size as f64
    }

    /// The object's kind (`commit`, `tree`, `blob`, `tag`).
    #[napi(getter)]
    pub fn kind(&self) -> String {
        self.kind.clone()
    }

    /// Current cursor position in bytes.
    #[napi(getter)]
    pub fn position(&self) -> f64 {
        self.pos as f64
    }

    /// Read up to `max_bytes` bytes from the current cursor, advancing it. Returns an empty `Buffer`
    /// at EOF.
    #[napi]
    pub fn read(&mut self, max_bytes: f64) -> napi::Result<Buffer> {
        let max = if max_bytes <= 0.0 {
            0
        } else {
            max_bytes as usize
        };
        if max == 0 {
            return Ok(Buffer::from(Vec::new()));
        }
        let start = self.pos as usize;
        let end = std::cmp::min(start + max, self.data.len());
        let chunk = self.data[start..end].to_vec();
        self.pos = end as u64;
        Ok(Buffer::from(chunk))
    }

    /// Seek to a position, returning the new absolute offset. `whence` selects the reference point
    /// (start / current / end); negative `offset` with `Current`/`End` is allowed. Errors if the
    /// resulting position would lie outside `[0, size]`.
    #[napi]
    pub fn seek(&mut self, offset: f64, whence: SeekWhence) -> napi::Result<f64> {
        let base: i64 = match whence {
            SeekWhence::Start => 0,
            SeekWhence::Current => self.pos as i64,
            SeekWhence::End => self.size as i64,
        };
        let new_pos = base
            .checked_add(offset as i64)
            .ok_or_else(|| err_str("buf_reader_seek", "seek overflow"))?;
        if new_pos < 0 || new_pos > self.size as i64 {
            return Err(err_str("buf_reader_seek", "seek out of bounds"));
        }
        self.pos = new_pos as u64;
        Ok(self.pos as f64)
    }
}

/// A buffered writer over a [`BufferedObjectStore`]: bytes are accumulated and written as a single
/// content-addressed object on [`shutdown`](Self::shutdown).
#[napi]
pub struct ObjectStoreBufWriter {
    store: BufferedObjectStore,
    kind: String,
    buf: Vec<u8>,
}

#[napi]
impl ObjectStoreBufWriter {
    /// Open a buffered writer. `kind` is the object kind (`commit`, `tree`, `blob`, `tag`); the
    /// object's id is computed from its final bytes on `shutdown`.
    #[napi(constructor)]
    pub fn new(
        store: &BufferedObjectStore,
        kind: String,
        _options: Option<BufWriterOptions>,
    ) -> napi::Result<Self> {
        let inner = store.inner.clone();
        Ok(Self {
            store: BufferedObjectStore { inner },
            kind,
            buf: Vec::new(),
        })
    }

    /// Append `data`, returning the number of bytes written (always `data.length`).
    #[napi]
    pub fn write(&mut self, data: Buffer) -> napi::Result<u32> {
        self.buf.extend_from_slice(&data);
        Ok(data.len() as u32)
    }

    /// Flush buffered data. For the local single-put path this is a no-op; the actual write happens
    /// on `shutdown`.
    #[napi]
    pub fn flush(&mut self) -> napi::Result<()> {
        Ok(())
    }

    /// Finalize: write the buffered bytes as a single object of `kind` and return its id. The writer
    /// is consumed — further `write` calls are harmless (they accumulate into a fresh buffer) but
    /// should be avoided.
    #[napi]
    pub fn shutdown(&mut self) -> napi::Result<String> {
        let data = std::mem::take(&mut self.buf);
        ObjectStore::write_object(&self.store, self.kind.clone(), Buffer::from(data))
    }

    /// Discard any buffered state without writing.
    #[napi]
    pub fn abort(&mut self) -> napi::Result<()> {
        self.buf.clear();
        Ok(())
    }
}
