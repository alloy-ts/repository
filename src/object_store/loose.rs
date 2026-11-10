use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use gix::ObjectId;
use gix::object::Kind;

use crate::object_store::base::GenericObjectStore;
use crate::object_store::base::ObjectStore;
use crate::object_store::{
    ObjectStoreOptions, ObjectStoreReader, ObjectStoreWriter, PrefixLookup, WriterTarget,
};

/// A persistent, prolly-backed object database — the on-disk lower-level store.
///
/// This is the successor to the old git-loose-object store: instead of writing one
/// zlib file per object under an `objects/` fanout, it stores objects in a content-addressed,
/// copy-on-write Prolly tree over a [`FileNodeStore`](prolly::FileNodeStore). That gives:
///
/// - **Persistence across restarts**: the current tree root is persisted under the name `"head"`
///   (via [`save_root`](Self::save_root)); a store reopened at the same directory reloads it.
/// - **Cheap, isolated forks**: [`fork`](Self::fork) is `O(1)` and shares every underlying node
///   with the parent (write a snapshot with [`save_root`](Self::save_root) to make it durable).
/// - **Cross-snapshot dedup**: identical subtrees across forks/stores are deduplicated on disk.
///
/// The trade-off versus a plain git-loose store: objects no longer live as individual git-readable
/// files, so [`object_path`](Self::object_path) errors (there is no per-object loose file) and a
/// real `.git/objects` directory cannot be read directly.
///
/// It implements the shared [`ObjectStore`] base trait, so its napi surface is identical to the
/// other store variants.
#[napi]
pub struct LooseObjectStore {
    inner: GenericObjectStore,
}

impl ObjectStore for LooseObjectStore {
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
impl LooseObjectStore {
    /// Open the persistent object database rooted at `objects_directory` (created if absent).
    ///
    /// `object_hash` is `"sha1"` (default) or `"sha256"`. `options` is accepted for API
    /// compatibility with the previous git-loose backend but is currently unused: the prolly
    /// backend manages its own node compression and allocation.
    ///
    /// If a previous store was saved at this directory (via [`save_root`](Self::save_root)),
    /// its root is reloaded automatically; otherwise the store starts empty.
    #[napi(constructor)]
    pub fn new(
        objects_directory: String,
        object_hash: Option<String>,
        options: Option<ObjectStoreOptions>,
    ) -> napi::Result<Self> {
        let _ = options;
        let inner = GenericObjectStore::new(objects_directory, object_hash)?;
        Ok(LooseObjectStore { inner })
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

    /// Serialise every object currently held in the store into a git packfile `Buffer`.
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
    pub fn fork(&self) -> LooseObjectStore {
        LooseObjectStore {
            inner: self.inner.fork(),
        }
    }

    /// Persist the current tree root under the name `"head"` so a later
    /// [`LooseObjectStore::new`] at the same directory reloads it.
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
