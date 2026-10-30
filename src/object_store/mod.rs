//! Object stores: a pack-aware [`ObjectDatabase`], the bare [`ObjectHandle`], and two variants of a
//! lower-level store — [`LooseObjectStore`] (one file per object on disk) and [`MemoryObjectStore`]
//! (a `Mutex`-guarded map, mirroring libgit2's `git_mempack`). Both store variants implement the
//! shared [`ObjectStore`](crate::object_store::base::ObjectStore) base trait (in `base.rs`), which
//! factors out their common napi surface.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use gix::objs::{Exists, Find, Write};
use gix::odb::Header as OdbHeader;
use gix::zlib::Compression;

use crate::error::{err, err_str, parse_oid};
use crate::object::{kind_to_string, string_to_kind};

pub mod arrow;
pub mod base;
pub mod client;
pub mod fjall;
pub mod loose;
pub mod memory;
pub mod stream;

/// Lower-cased string form of a hash kind (`"sha1"` / `"sha256"`). `gix::hash::Kind` is
/// `#[non_exhaustive]`, so any future kind is reported as `sha256`.
fn hash_kind_format(kind: gix::hash::Kind) -> &'static str {
    match kind {
        gix::hash::Kind::Sha1 => "sha1",
        _ => "sha256",
    }
}

/// Resolve an optional object-hash string to a [`gix::hash::Kind`], defaulting to SHA-1.
fn object_kind_from_option(hash: Option<&str>) -> napi::Result<gix::hash::Kind> {
    match hash {
        None | Some("sha1") => Ok(gix::hash::Kind::Sha1),
        Some(other) => crate::object::parse_hash_kind_ci(other).ok_or_else(|| {
            err_str(
                "object_hash",
                format!("unsupported object hash {other} (expected sha1 or sha256)"),
            )
        }),
    }
}

/// Git's loose-object-map (`LMAP`) format ids for SHA-1 and SHA-256.
const LMAP_SHA1_FORMAT_ID: u32 = 0x7368_6131;
const LMAP_SHA256_FORMAT_ID: u32 = 0x7332_3536;

/// Read a git loose-object-map file (`<objects>/loose-object-map`) and return the
/// `(compat_oid, storage_oid)` pairs that let a SHA-256 store also resolve SHA-1 ids
/// (git's `extensions.compatObjectFormat = sha1`). Returns an empty map when the file is
/// absent or malformed — a SHA-256 *canonical* repo has no compatibility ids to map, so the
/// empty map is correct there.
fn read_loose_object_map(
    objects_directory: &str,
) -> Vec<(gix::hash::ObjectId, gix::hash::ObjectId)> {
    let path = std::path::Path::new(objects_directory).join("loose-object-map");
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    parse_lmap(&bytes).unwrap_or_default()
}

/// Parse git's `LMAP` file format (see git's `loose-object-map.c`). The header is `"LMAP"`, a
/// version, sizes, then one `object_format_header` per hash algorithm; per algorithm it stores a
/// shortened-id table, a full-id table, and (for the storage algorithm) a 4-byte metadata table.
/// We only need the two full-id tables to build `(compat_oid, storage_oid)` pairs.
fn parse_lmap(bytes: &[u8]) -> Option<Vec<(gix::hash::ObjectId, gix::hash::ObjectId)>> {
    if bytes.get(0..4)? != b"LMAP" {
        return None;
    }
    if u32_at(bytes, 4)? != 1 {
        return None;
    }
    let nitems = usize::try_from(u32_at(bytes, 12)?).ok()?;
    let nobj_formats = usize::try_from(u32_at(bytes, 16)?).ok()?;
    if nobj_formats < 2 {
        return None;
    }
    let mut offset = 20usize;
    let mut headers: Vec<(u32, usize, usize)> = Vec::with_capacity(nobj_formats);
    for _ in 0..nobj_formats {
        if offset + 16 > bytes.len() {
            return None;
        }
        let format_id = u32_at(bytes, offset)?;
        let shortened_len = usize::try_from(u32_at(bytes, offset + 4)?).ok()?;
        let data_off = usize::try_from(u64_at(bytes, offset + 8)?).ok()?;
        headers.push((format_id, shortened_len, data_off));
        offset += 16;
    }
    // Trailing offset (validated loosely).
    let _ = u64_at(bytes, offset)?;

    // The storage (canonical) hash is SHA-256 and the compatibility hash is SHA-1.
    let storage_hdr = headers
        .iter()
        .find(|(id, _, _)| *id == LMAP_SHA256_FORMAT_ID)?;
    let compat_hdr = headers
        .iter()
        .find(|(id, _, _)| *id == LMAP_SHA1_FORMAT_ID)?;
    let (_, storage_short, storage_data) = *storage_hdr;
    let (_, compat_short, compat_data) = *compat_hdr;

    let storage_full = storage_data.checked_add(storage_short.checked_mul(nitems)?)?;
    let compat_full = compat_data.checked_add(compat_short.checked_mul(nitems)?)?;
    let storage_len = 32usize;
    let compat_len = 20usize;

    let mut out = Vec::with_capacity(nitems);
    for i in 0..nitems {
        let s = bytes.get(storage_full + i * storage_len..storage_full + (i + 1) * storage_len)?;
        let c = bytes.get(compat_full + i * compat_len..compat_full + (i + 1) * compat_len)?;
        let storage_oid = gix::hash::ObjectId::try_from(s).ok()?;
        let compat_oid = gix::hash::ObjectId::try_from(c).ok()?;
        out.push((compat_oid, storage_oid));
    }
    Some(out)
}

#[inline]
fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    let s = bytes.get(offset..offset + 4)?;
    Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

#[inline]
fn u64_at(bytes: &[u8], offset: usize) -> Option<u64> {
    let s = bytes.get(offset..offset + 8)?;
    Some(u64::from_be_bytes([
        s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
    ]))
}

/// Options for constructing a loose or memory [`ObjectStore`](crate::object_store::base::ObjectStore) variant.
#[napi(object)]
pub struct ObjectStoreOptions {
    /// Maximum size (in bytes) of a single allocation caused by a loose object read/write.
    /// `null` means no additional limit.
    pub alloc_limit_bytes: Option<u32>,
    /// zlib compression level (0–9). `null` falls back to the git default (`BEST_SPEED`, level 1).
    pub compression_level: Option<u32>,
}

/// The result of resolving a short object id prefix.
#[napi(object)]
pub struct PrefixLookup {
    /// `true` if exactly one object matches the prefix.
    pub found: bool,
    /// `true` if more than one object matches the prefix (it is ambiguous).
    pub ambiguous: bool,
    /// The matched object id, present iff `found` is `true`.
    pub id: Option<String>,
}

/// The target a [`ObjectStoreWriter`] flushes its buffered object into.
pub(crate) enum WriterTarget {
    Loose(Mutex<gix::odb::loose::Store>),
    Handle(Mutex<HandleStore>),
    Memory(Arc<crate::object_store::memory::InMemoryStore>),
    Arrow(Arc<crate::object_store::arrow::ArrowStore>),
}

/// A streaming reader over an object's raw, decompressed content.
///
/// Returned by [`LooseObjectStore::reader`], [`MemoryObjectStore::reader`], [`ObjectHandle::reader`]
/// and [`ObjectDatabase::reader`]. The object is loaded in full on creation; [`read`](Self::read)
/// hands it back in chunks.
#[napi]
pub struct ObjectStoreReader {
    data: Vec<u8>,
    kind: String,
    pos: usize,
}

#[napi]
impl ObjectStoreReader {
    /// Total size of the object's content in bytes.
    #[napi(getter)]
    pub fn size(&self) -> f64 {
        self.data.len() as f64
    }

    /// The object kind (`"blob"`, `"tree"`, `"commit"`, or `"tag"`).
    #[napi(getter)]
    pub fn kind(&self) -> String {
        self.kind.clone()
    }

    /// Read up to `max_bytes` bytes of object content, returning a `Buffer`.
    /// Returns an empty `Buffer` once the object has been fully consumed.
    #[napi]
    pub fn read(&mut self, max_bytes: f64) -> Buffer {
        let remaining = self.data.len() - self.pos;
        let n = (max_bytes as usize).min(remaining);
        let end = self.pos + n;
        let out = Buffer::from(self.data[self.pos..end].to_vec());
        self.pos = end;
        out
    }
}

/// A streaming writer that buffers object content and flushes it on `finalize`.
///
/// Returned by [`LooseObjectStore::writer`], [`MemoryObjectStore::writer`], [`ObjectHandle::writer`]
/// and [`ObjectDatabase::writer`].
#[napi]
pub struct ObjectStoreWriter {
    target: WriterTarget,
    kind: gix::object::Kind,
    buf: Vec<u8>,
    expected_size: Option<usize>,
}

#[napi]
impl ObjectStoreWriter {
    /// Append a chunk of object content. Fails if this would exceed the size declared at creation.
    #[napi]
    pub fn write(&mut self, data: Buffer) -> napi::Result<()> {
        if let Some(expected) = self.expected_size {
            if self.buf.len() + data.len() > expected {
                return Err(err_str(
                    "write",
                    format!(
                        "writing beyond declared size: have {} + {} > {}",
                        self.buf.len(),
                        data.len(),
                        expected
                    ),
                ));
            }
        }
        self.buf.extend_from_slice(&data);
        Ok(())
    }

    /// Finalize the object, returning its hex id. Fails if the total written
    /// size does not match the size declared when the writer was created.
    #[napi]
    pub fn finalize(&mut self) -> napi::Result<String> {
        if let Some(expected) = self.expected_size {
            if self.buf.len() != expected {
                return Err(err_str(
                    "finalize",
                    format!(
                        "wrote {} bytes but declared size was {}",
                        self.buf.len(),
                        expected
                    ),
                ));
            }
        }
        let id = match &self.target {
            WriterTarget::Loose(store) => {
                let store = store.lock().unwrap();
                gix::objs::Write::write_buf(&*store, self.kind, &self.buf)
                    .map_err(|e| err("finalize", e))?
            }
            WriterTarget::Handle(handle) => {
                let handle = handle.lock().unwrap();
                gix::objs::Write::write_buf(&*handle, self.kind, &self.buf)
                    .map_err(|e| err("finalize", e))?
            }
            WriterTarget::Memory(store) => store.write_object(self.kind, &self.buf)?,
            WriterTarget::Arrow(store) => store.write_object(self.kind, &self.buf)?,
        };
        Ok(id.to_string())
    }
}

/// Options for constructing an [`ObjectDatabase`].
#[napi(object)]
pub struct ObjectDatabaseOptions {
    /// Object hash kind: `"sha1"` (default). SHA-256 requires the gix `sha256` feature.
    pub object_hash: Option<String>,
    /// Compatibility object format for a SHA-256 store that also accepts SHA-1 ids
    /// (`extensions.compatObjectFormat`). Only `"sha1"` is valid; when set, the store can
    /// resolve the historical SHA-1 ids of its objects (git's SHA-256 + SHA-1 compat mode).
    pub compat_object_format: Option<String>,
    /// Fixed number of index/pack slots (`Slots::Given`). When `null` and neither
    /// `slots_multiplier` nor `slots_minimum` is set, a sensible default is computed from disk state.
    pub slots: Option<u32>,
    /// When `slots` is `null`, size the slot map via `Slots::AsNeededByDiskState` using this safety
    /// multiplier over the on-disk index count (e.g. `1.1` = 10% headroom).
    pub slots_multiplier: Option<f64>,
    /// Minimum slot count when using `Slots::AsNeededByDiskState`.
    pub slots_minimum: Option<u32>,
    /// Whether multi-pack indices are used. Defaults to `true`.
    pub use_multi_pack_index: Option<bool>,
    /// Maximum size (in bytes) of a single allocation caused by on-disk pack data. `null` = no limit.
    pub alloc_limit_bytes: Option<u32>,
    /// zlib compression level (0–9) for loose objects. `null` = git default (`BEST_SPEED`).
    pub loose_compression: Option<u32>,
    /// Pack cache kind: only `"none"` (gix's `Never` cache) is available without the gix
    /// `pack-cache-lru-*` feature.
    pub pack_cache: Option<String>,
    /// Object cache kind: only `"none"` (gix's `Never` cache) is available without the gix
    /// `pack-cache-lru-*` feature.
    pub object_cache: Option<String>,
    /// Process current directory used to resolve alternate object-database paths. `null` lets gix obtain it.
    pub current_dir: Option<String>,
}

/// The pack-aware, repository-independent object database wrapped in gix's caching layer.
///
/// Wraps `gix::odb::Cache<Handle<Arc<gix::odb::Store>>>`. Unlike the loose/memory
/// [`ObjectStore`](crate::object_store::base::ObjectStore) variants, this consults packs and alternates in addition to loose objects, and
/// supports pack/object caches to speed up repeated or delta-chain decodes. Use
/// [`ObjectDatabase::into_inner`] to drop the cache and obtain the bare [`ObjectHandle`].
///
/// The wrapped `Cache` is not `Sync` (it holds `RefCell`s), so it is kept behind a `Mutex` here.
#[napi]
pub struct ObjectDatabase {
    inner: Mutex<CachedStore>,
    path: String,
    object_hash: gix::hash::Kind,
    compat_object_format: Option<gix::hash::Kind>,
}

#[napi]
impl ObjectDatabase {
    /// Open the object database rooted at `objects_directory` (typically `<repo>/.git/objects`),
    /// building the pack/loose store, a handle, and a (initially uncached unless requested) cache.
    #[napi(constructor)]
    pub fn new(
        objects_directory: String,
        options: Option<ObjectDatabaseOptions>,
    ) -> napi::Result<Self> {
        let options = options.unwrap_or(ObjectDatabaseOptions {
            object_hash: None,
            compat_object_format: None,
            slots: None,
            slots_multiplier: None,
            slots_minimum: None,
            use_multi_pack_index: None,
            alloc_limit_bytes: None,
            loose_compression: None,
            pack_cache: None,
            object_cache: None,
            current_dir: None,
        });
        let object_hash = match options.object_hash.as_deref() {
            None | Some("sha1") => gix::hash::Kind::Sha1,
            Some(other) => match crate::object::parse_hash_kind_ci(other) {
                Some(kind) => kind,
                None => {
                    return Err(err_str(
                        "ObjectDatabase::new",
                        format!("unsupported object hash {other} (expected sha1 or sha256)"),
                    ));
                }
            },
        };
        let compat_object_format = match options.compat_object_format.as_deref() {
            None => None,
            Some("sha1") => Some(gix::hash::Kind::Sha1),
            Some(other) => {
                return Err(err_str(
                    "ObjectDatabase::new",
                    format!("unsupported compatObjectFormat '{other}' (only 'sha1' is supported)"),
                ));
            }
        };
        // gix's `Store::at_opts` accepts a `(compat_oid, storage_oid)` map so a SHA-256 store can
        // resolve SHA-1 ids. It is only meaningful when storage is SHA-256 and compat is SHA-1;
        // otherwise there is nothing to map. The map is populated best-effort from the repo's
        // `loose-object-map` file (absent for SHA-256 canonical repos, which is correct).
        let compat_map: Vec<(gix::hash::ObjectId, gix::hash::ObjectId)> = match compat_object_format
        {
            Some(gix::hash::Kind::Sha1) if object_hash == gix::hash::Kind::Sha256 => {
                read_loose_object_map(&objects_directory)
            }
            Some(_) => {
                return Err(err_str(
                    "ObjectDatabase::new",
                    "compatObjectFormat is only valid for a SHA-256 store with SHA-1 compatibility",
                ));
            }
            None => Vec::new(),
        };
        let init_options = gix::odb::store::init::Options {
            slots: match (
                options.slots,
                options.slots_multiplier,
                options.slots_minimum,
            ) {
                (Some(n), _, _) => gix::odb::store::init::Slots::Given(n as u16),
                (None, multiplier, minimum) => gix::odb::store::init::Slots::AsNeededByDiskState {
                    multiplier: multiplier.unwrap_or(1.1) as f32,
                    minimum: minimum.unwrap_or(32) as usize,
                },
            },
            use_multi_pack_index: options.use_multi_pack_index.unwrap_or(true),
            alloc_limit_bytes: options.alloc_limit_bytes.map(|b| b as usize),
            current_dir: options.current_dir.clone().map(std::path::PathBuf::from),
            loose_compression: match options.loose_compression {
                Some(level) => Compression::new(level as i32).unwrap_or(Compression::BEST_SPEED),
                None => Compression::BEST_SPEED,
            },
        };
        let store = gix::odb::Store::at_opts(
            objects_directory.clone().into(),
            object_hash,
            &mut compat_map.into_iter(),
            init_options,
        )
        .map_err(|e| err("ObjectDatabase::new", e))?;
        let handle = Arc::new(store).to_handle_arc();
        let mut cache = gix::odb::Cache::from(handle);
        if options.pack_cache.as_deref() == Some("none") {
            cache.set_pack_cache(|| Box::new(gix::odb::pack::cache::Never));
        }
        if options.object_cache.as_deref() == Some("none") {
            cache.set_object_cache(|| Box::new(gix::odb::pack::cache::object::Never));
        }
        Ok(ObjectDatabase {
            inner: Mutex::new(cache),
            path: objects_directory,
            object_hash,
            compat_object_format,
        })
    }

    /// `true` if an object cache constructor/payload is installed.
    #[napi]
    pub fn has_object_cache(&self) -> bool {
        self.inner.lock().unwrap().has_object_cache()
    }

    /// `true` if a pack cache constructor/payload is installed.
    #[napi]
    pub fn has_pack_cache(&self) -> bool {
        self.inner.lock().unwrap().has_pack_cache()
    }

    /// Install (or replace) the pack cache. Currently only `"none"` (gix's `Never` cache) is
    /// supported; an LRU cache requires enabling the gix `pack-cache-lru-*` feature.
    #[napi]
    pub fn set_pack_cache(&mut self, kind: String) -> napi::Result<()> {
        let mut cache = self.inner.lock().unwrap();
        match kind.as_str() {
            "none" => cache.set_pack_cache(|| Box::new(gix::odb::pack::cache::Never)),
            other => {
                return Err(err_str(
                    "set_pack_cache",
                    format!("unsupported pack cache kind {other}"),
                ));
            }
        }
        Ok(())
    }

    /// Install (or replace) the object cache. See [`set_pack_cache`](Self::set_pack_cache) for the
    /// available kinds.
    #[napi]
    pub fn set_object_cache(&mut self, kind: String) -> napi::Result<()> {
        let mut cache = self.inner.lock().unwrap();
        match kind.as_str() {
            "none" => cache.set_object_cache(|| Box::new(gix::odb::pack::cache::object::Never)),
            other => {
                return Err(err_str(
                    "set_object_cache",
                    format!("unsupported object cache kind {other}"),
                ));
            }
        }
        Ok(())
    }

    /// Remove the pack cache (and its constructor) from this instance.
    #[napi]
    pub fn unset_pack_cache(&mut self) {
        self.inner.lock().unwrap().unset_pack_cache();
    }

    /// Remove the object cache (and its constructor) from this instance.
    #[napi]
    pub fn unset_object_cache(&mut self) {
        self.inner.lock().unwrap().unset_object_cache();
    }

    /// Build a transient [`ObjectHandle`] view over the same underlying store, so the cached
    /// database can expose the handle's lookup/try_find/try_header/contains methods.
    fn as_handle(&self) -> ObjectHandle {
        ObjectHandle {
            inner: std::sync::Mutex::new(self.inner.lock().unwrap().clone().into_inner()),
            path: self.path.clone(),
            object_hash: self.object_hash,
        }
    }

    /// Whether an object with `id` exists in the database.
    #[napi]
    pub fn contains(&self, id: String) -> bool {
        self.as_handle().contains(id)
    }

    /// Resolve an object id by unique prefix, returning the id and ambiguity flag.
    #[napi]
    pub fn lookup_prefix(&self, id: String) -> napi::Result<PrefixLookup> {
        self.as_handle().lookup_prefix(id)
    }

    /// Read the full object data for `id`, or `null` if it does not exist.
    #[napi]
    pub fn try_find(&self, id: String) -> napi::Result<Option<crate::object::ObjectData>> {
        self.as_handle().try_find(id)
    }

    /// Read just the object header (kind + size) for `id`, or `null` if it does not exist.
    #[napi]
    pub fn try_header(&self, id: String) -> napi::Result<Option<crate::object::ObjectHeader>> {
        self.as_handle().try_header(id)
    }

    /// Open a streaming reader for `id`. See [`ObjectHandle::reader`].
    #[napi]
    pub fn reader(&self, id: String) -> napi::Result<ObjectStoreReader> {
        self.as_handle().reader(id)
    }

    /// Open a streaming writer for a new object of `kind` and `size`. See [`ObjectHandle::writer`].
    #[napi]
    pub fn writer(&self, kind: String, size: f64) -> napi::Result<ObjectStoreWriter> {
        self.as_handle().writer(kind, size)
    }

    /// Return a snapshot of the store's resource usage.
    #[napi]
    pub fn metrics(&self) -> napi::Result<StoreMetrics> {
        self.as_handle().metrics()
    }

    /// Return the hex ids of every object in the database.
    #[napi]
    pub fn entries(&self) -> napi::Result<Vec<String>> {
        self.as_handle().entries()
    }

    /// Tally object counts and sizes across the whole database.
    #[napi]
    pub fn statistics(&self) -> napi::Result<ObjectStatistics> {
        self.as_handle().statistics()
    }

    /// Return combined store information.
    #[napi]
    pub fn info(&self) -> napi::Result<StoreInfo> {
        self.as_handle().info()
    }

    /// Dissolve this instance, discarding all caches, and return the bare object handle.
    #[napi]
    pub fn into_inner(&self) -> ObjectHandle {
        let cache = self.inner.lock().unwrap().clone();
        ObjectHandle {
            inner: Mutex::new(cache.into_inner()),
            path: self.path.clone(),
            object_hash: self.object_hash,
        }
    }

    /// No-op for the already-`Arc`-backed handle; returns a clone of this cached database.
    #[napi]
    pub fn into_arc(&self) -> napi::Result<ObjectDatabase> {
        let cache = self.inner.lock().unwrap().clone();
        Ok(ObjectDatabase {
            inner: Mutex::new(cache),
            path: self.path.clone(),
            object_hash: self.object_hash,
            compat_object_format: self.compat_object_format,
        })
    }

    /// Return the `objects` directory path.
    #[napi(getter)]
    pub fn path(&self) -> String {
        self.path.clone()
    }

    /// Return the hash kind this database uses (`"sha1"` or `"sha256"`).
    #[napi(getter)]
    pub fn object_hash(&self) -> String {
        self.object_hash.to_string()
    }

    /// The object format this database uses, lower-cased (`"sha1"` or `"sha256"`), for parity
    /// with [`crate::repository::Repository::objectFormat`].
    #[napi(getter)]
    pub fn object_format(&self) -> String {
        hash_kind_format(self.object_hash).to_string()
    }

    /// The compatibility object format for a SHA-256 store that also accepts SHA-1 ids
    /// (`extensions.compatObjectFormat`), or `null` when the store only understands its
    /// canonical hash.
    #[napi(getter)]
    pub fn compat_object_format(&self) -> Option<String> {
        self.compat_object_format
            .map(hash_kind_format)
            .map(str::to_string)
    }
}

/// A record describing one structural element of an object database, as returned by
/// [`ObjectHandle::structure`].
#[napi(object)]
pub struct StoreRecord {
    /// One of `"loose"`, `"index"`, `"multi_index"`, `"empty"`.
    pub kind: String,
    /// Path of the loose db, index, or multi-index file (absent for `"empty"`).
    pub path: Option<String>,
    /// Number of loose objects, for `"loose"` records.
    pub num_objects: Option<u32>,
    /// Load state of an index/multi-index: `"loaded"`, `"disposable"`, or `"unloaded"`.
    pub state: Option<String>,
}

/// A snapshot of the object database's resource usage (mirrors `gix::odb::store::Metrics`).
#[napi(object)]
#[derive(Default)]
pub struct StoreMetrics {
    /// Total number of handles which can be used to access object information.
    pub num_handles: f64,
    /// Number of refreshes performed to reconcile with the on-disk ODB state.
    pub num_refreshes: f64,
    /// Indices currently open and returned to handles.
    pub open_reachable_indices: f64,
    /// Reachable, known indices that are not yet opened.
    pub known_reachable_indices: f64,
    /// Packs open in memory and returned to handles.
    pub open_reachable_packs: f64,
    /// Reachable packs that are not yet opened.
    pub known_packs: f64,
    /// Slots that are empty.
    pub unused_slots: f64,
    /// Unreachable indices still using slots (not returned to new handles).
    pub unreachable_indices: f64,
    /// Unreachable mapped pack data files still using slots.
    pub unreachable_packs: f64,
    /// Loose object databases currently available for retrieval (more than one with alternates).
    pub loose_dbs: f64,
}

/// Aggregated counts and encoded sizes of all objects, grouped by kind and by loose/packed storage.
/// Mirrors gix's `statistics` aggregation.
#[napi(object)]
#[derive(Default)]
pub struct ObjectStatistics {
    /// Total objects counted (loose + packed).
    pub total_objects: f64,
    /// Objects found as loose objects.
    pub loose_objects: f64,
    /// Objects found in packs.
    pub packed_objects: f64,
    /// Packed objects that are deltas (i.e. `num_deltas > 0`).
    pub packed_delta_objects: f64,
    /// Sum of the delta chain lengths across all packed objects.
    pub total_delta_chain_length: f64,
    /// Number of commit objects.
    pub commits: f64,
    /// Encoded size (bytes) of all commit objects.
    pub commits_size: f64,
    /// Number of tree objects.
    pub trees: f64,
    /// Encoded size (bytes) of all tree objects.
    pub trees_size: f64,
    /// Number of tag objects.
    pub tags: f64,
    /// Encoded size (bytes) of all tag objects.
    pub tags_size: f64,
    /// Number of blob objects.
    pub blobs: f64,
    /// Encoded size (bytes) of all blob objects.
    pub blobs_size: f64,
}

impl ObjectStatistics {
    fn consume(&mut self, h: gix::odb::find::Header) {
        self.total_objects += 1.0;
        match h {
            gix::odb::find::Header::Loose { kind, size } => {
                self.loose_objects += 1.0;
                self.count(kind, size as f64);
            }
            gix::odb::find::Header::Packed(out) => {
                self.packed_objects += 1.0;
                if out.num_deltas > 0 {
                    self.packed_delta_objects += 1.0;
                }
                self.total_delta_chain_length += out.num_deltas as f64;
                self.count(out.kind, out.object_size as f64);
            }
        }
    }

    fn count(&mut self, kind: gix::object::Kind, size: f64) {
        match kind {
            gix::object::Kind::Commit => {
                self.commits += 1.0;
                self.commits_size += size;
            }
            gix::object::Kind::Tree => {
                self.trees += 1.0;
                self.trees_size += size;
            }
            gix::object::Kind::Tag => {
                self.tags += 1.0;
                self.tags_size += size;
            }
            gix::object::Kind::Blob => {
                self.blobs += 1.0;
                self.blobs_size += size;
            }
        }
    }
}

/// Combined store information (mirrors gix's `info`): location, hash, multi-pack-index flag,
/// structural records, and resource metrics.
#[napi(object)]
pub struct StoreInfo {
    /// The objects directory path.
    pub path: String,
    /// The object hash kind (`"sha1"` or `"sha256"`).
    pub object_hash: String,
    /// Whether multi-pack indices are used.
    pub use_multi_pack_index: bool,
    /// Structural records describing the database's files and their load state.
    pub structure: Vec<StoreRecord>,
    /// Resource usage metrics.
    pub metrics: StoreMetrics,
}

fn index_state_to_string(state: gix::odb::store::structure::IndexState) -> String {
    match state {
        gix::odb::store::structure::IndexState::Loaded => "loaded",
        gix::odb::store::structure::IndexState::Disposable => "disposable",
        gix::odb::store::structure::IndexState::Unloaded => "unloaded",
    }
    .to_string()
}

/// The pack-aware, repository-independent object database handle:
/// `gix::odb::store::Handle<Arc<gix::odb::Store>>`.
type HandleStore = gix::odb::store::Handle<Arc<gix::odb::Store>>;
/// The cached object database: `gix::odb::Cache<Handle<Arc<Store>>>`.
type CachedStore = gix::odb::Cache<HandleStore>;

/// A database for reading and writing objects, consulting loose objects, packs, and alternates.
///
/// Wraps `gix::odb::store::Handle<Arc<gix::odb::Store>>`. Unlike the loose/memory
/// [`ObjectStore`](crate::object_store::base::ObjectStore) variants, this views the full repository object database (loose + packs +
/// alternates) without the caching layer of [`ObjectDatabase`].
#[napi]
pub struct ObjectHandle {
    inner: Mutex<HandleStore>,
    path: String,
    object_hash: gix::hash::Kind,
}

#[napi]
impl ObjectHandle {
    /// Whether an object with the given id exists in this database (loose or packed).
    #[napi]
    pub fn contains(&self, id: String) -> bool {
        match parse_oid(&id) {
            Ok(oid) => Exists::exists(&*self.inner.lock().unwrap(), &oid),
            Err(_) => false,
        }
    }

    /// Resolve a short object id `prefix` (hex) to a unique object.
    ///
    /// Returns `found: false` if no object matches, `ambiguous: true` if more than one matches,
    /// and `id` set when exactly one matches.
    #[napi]
    pub fn lookup_prefix(&self, prefix: String) -> napi::Result<PrefixLookup> {
        let prefix = gix::hash::Prefix::from_hex(&prefix).map_err(|e| err("lookup_prefix", e))?;
        match self.inner.lock().unwrap().lookup_prefix(prefix, None) {
            Ok(Some(Ok(oid))) => Ok(PrefixLookup {
                found: true,
                ambiguous: false,
                id: Some(oid.to_string()),
            }),
            Ok(Some(Err(()))) => Ok(PrefixLookup {
                found: false,
                ambiguous: true,
                id: None,
            }),
            Ok(None) => Ok(PrefixLookup {
                found: false,
                ambiguous: false,
                id: None,
            }),
            Err(e) => Err(err("lookup_prefix", e)),
        }
    }

    /// Read an object's raw, decompressed body (without the loose-object header) and kind.
    /// Returns `null` if the object is missing.
    #[napi]
    pub fn try_find(&self, id: String) -> napi::Result<Option<crate::object::ObjectData>> {
        let oid = parse_oid(&id)?;
        let mut buf = Vec::new();
        let data = Find::try_find(&*self.inner.lock().unwrap(), &oid, &mut buf)
            .map_err(|e| err("try_find", e))?;
        Ok(data.map(|d| crate::object::ObjectData {
            kind: kind_to_string(d.kind),
            data: Buffer::from(d.data.to_vec()),
        }))
    }

    /// Read an object's header (kind + size) without decoding its full content.
    /// Returns `null` if the object is missing.
    #[napi]
    pub fn try_header(&self, id: String) -> napi::Result<Option<crate::object::ObjectHeader>> {
        let oid = parse_oid(&id)?;
        let header = OdbHeader::try_header(&*self.inner.lock().unwrap(), &oid)
            .map_err(|e| err("try_header", e))?;
        Ok(header.map(crate::object::ObjectHeader::from))
    }

    /// Open a streaming reader for the object identified by `id`.
    ///
    /// The object is loaded in full and delivered incrementally by [`ObjectStoreReader::read`].
    #[napi]
    pub fn reader(&self, id: String) -> napi::Result<ObjectStoreReader> {
        let oid = parse_oid(&id)?;
        let mut buf = Vec::new();
        let data = gix::objs::Find::try_find(&*self.inner.lock().unwrap(), &oid, &mut buf)
            .map_err(|e| err("reader", e))?;
        match data {
            Some(d) => Ok(ObjectStoreReader {
                data: d.data.to_vec(),
                kind: kind_to_string(d.kind),
                pos: 0,
            }),
            None => Err(err_str("reader", format!("object {id} not found"))),
        }
    }

    /// Open a streaming writer for a new object of `kind` (`commit`, `tree`, `blob`, `tag`) and
    /// total `size` in bytes. See [`ObjectStoreWriter`] for usage.
    #[napi]
    pub fn writer(&self, kind: String, size: f64) -> napi::Result<ObjectStoreWriter> {
        let kind = string_to_kind(&kind)?;
        let handle = self.inner.lock().unwrap().clone();
        Ok(ObjectStoreWriter {
            target: WriterTarget::Handle(Mutex::new(handle)),
            kind,
            buf: Vec::with_capacity(size as usize),
            expected_size: if size <= 0.0 {
                None
            } else {
                Some(size as usize)
            },
        })
    }

    /// Return information about all files known to the database and their loading state.
    #[napi]
    pub fn structure(&self) -> napi::Result<Vec<StoreRecord>> {
        let records = self
            .inner
            .lock()
            .unwrap()
            .store_ref()
            .structure()
            .map_err(|e| err("structure", e))?;
        Ok(records
            .into_iter()
            .map(|r| match r {
                gix::odb::store::structure::Record::LooseObjectDatabase {
                    objects_directory,
                    num_objects,
                } => StoreRecord {
                    kind: "loose".into(),
                    path: Some(objects_directory.to_string_lossy().into_owned()),
                    num_objects: Some(num_objects as u32),
                    state: None,
                },
                gix::odb::store::structure::Record::Index { path, state } => StoreRecord {
                    kind: "index".into(),
                    path: Some(path.to_string_lossy().into_owned()),
                    num_objects: None,
                    state: Some(index_state_to_string(state)),
                },
                gix::odb::store::structure::Record::MultiIndex { path, state } => StoreRecord {
                    kind: "multi_index".into(),
                    path: Some(path.to_string_lossy().into_owned()),
                    num_objects: None,
                    state: Some(index_state_to_string(state)),
                },
                gix::odb::store::structure::Record::Empty => StoreRecord {
                    kind: "empty".into(),
                    path: None,
                    num_objects: None,
                    state: None,
                },
            })
            .collect())
    }

    /// Return a snapshot of the store's resource usage (open handles, packs, indices, slots, …).
    #[napi]
    pub fn metrics(&self) -> napi::Result<StoreMetrics> {
        let m = self.inner.lock().unwrap().store_ref().metrics();
        Ok(StoreMetrics {
            num_handles: m.num_handles as f64,
            num_refreshes: m.num_refreshes as f64,
            open_reachable_indices: m.open_reachable_indices as f64,
            known_reachable_indices: m.known_reachable_indices as f64,
            open_reachable_packs: m.open_reachable_packs as f64,
            known_packs: m.known_packs as f64,
            unused_slots: m.unused_slots as f64,
            unreachable_indices: m.unreachable_indices as f64,
            unreachable_packs: m.unreachable_packs as f64,
            loose_dbs: m.loose_dbs as f64,
        })
    }

    /// Return the hex ids of every object in the database (loose and packed), in traversal order.
    /// Ids may be duplicated when an object exists in more than one pack or both packed and loose.
    #[napi]
    pub fn entries(&self) -> napi::Result<Vec<String>> {
        let guard = self.inner.lock().unwrap();
        let store = guard.store_ref();
        let iter = store.iter().map_err(|e| err("entries", e))?;
        let mut out = Vec::new();
        for id in iter {
            match id {
                Ok(id) => out.push(id.to_string()),
                Err(e) => return Err(err("entries", e)),
            }
        }
        Ok(out)
    }

    /// Tally object counts and encoded sizes across the whole database, grouped by kind and by
    /// loose/packed storage, and sum the delta chain lengths of packed objects.
    #[napi]
    pub fn statistics(&self) -> napi::Result<ObjectStatistics> {
        let guard = self.inner.lock().unwrap();
        let store = guard.store_ref();
        let iter = store.iter().map_err(|e| err("statistics", e))?;
        let mut stats = ObjectStatistics::default();
        for id in iter {
            let id = match id {
                Ok(id) => id,
                Err(e) => return Err(err("statistics", e)),
            };
            if let Some(header) = guard.try_header(&id).map_err(|e| err("statistics", e))? {
                stats.consume(header);
            }
        }
        Ok(stats)
    }

    /// Return combined store information: location, hash, multi-pack-index flag, structure, and metrics.
    #[napi]
    pub fn info(&self) -> napi::Result<StoreInfo> {
        let metrics = self.metrics()?;
        let structure = self.structure()?;
        let use_multi_pack_index = self
            .inner
            .lock()
            .unwrap()
            .store_ref()
            .use_multi_pack_index();
        Ok(StoreInfo {
            path: self.path.clone(),
            object_hash: self.object_hash.to_string(),
            use_multi_pack_index,
            structure,
            metrics,
        })
    }

    /// Return the list of `objects` directories contributed by alternate object databases
    /// (empty if there are no alternates).
    #[napi]
    pub fn alternate_db_paths(&self) -> napi::Result<Vec<String>> {
        let paths = self
            .inner
            .lock()
            .unwrap()
            .store_ref()
            .alternate_db_paths()
            .map_err(|e| err("alternate_db_paths", e))?;
        Ok(paths
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect())
    }

    /// Return the exact number of packed objects across all currently loaded indices.
    #[napi]
    pub fn packed_object_count(&self) -> napi::Result<i64> {
        let count = self
            .inner
            .lock()
            .unwrap()
            .packed_object_count()
            .map_err(|e| err("packed_object_count", e))?;
        Ok(count as i64)
    }

    /// Given an object `id` and an initial `hexLen`, extend the prefix length until it uniquely
    /// identifies a single object in the database. Returns the unambiguous prefix hex string, or
    /// `null` if no object matches the starting prefix. Errors if `hexLen` exceeds the hash length.
    #[napi]
    pub fn disambiguate_prefix(&self, id: String, hex_len: u32) -> napi::Result<Option<String>> {
        let oid = parse_oid(&id)?;
        let candidate =
            gix::odb::store::prefix::disambiguate::Candidate::new(oid, hex_len as usize)
                .map_err(|e| err("disambiguate_prefix", e))?;
        let prefix = self
            .inner
            .lock()
            .unwrap()
            .disambiguate_prefix(candidate)
            .map_err(|e| err("disambiguate_prefix", e))?;
        Ok(prefix.map(|p| p.to_string()))
    }

    /// Prevent packs from being unloaded even if they disappear from disk (e.g. during git maintenance).
    #[napi]
    pub fn prevent_pack_unload(&mut self) {
        self.inner.lock().unwrap().prevent_pack_unload();
    }

    /// Disable automatic ODB refreshes when an object cannot be found.
    #[napi]
    pub fn refresh_never(&mut self) {
        self.inner.lock().unwrap().refresh_never();
    }

    /// Return the current refresh mode: `"after_all_indices_loaded"` (default) or `"never"`.
    #[napi]
    pub fn refresh_mode(&mut self) -> String {
        match self.inner.lock().unwrap().refresh_mode() {
            gix::odb::store::RefreshMode::AfterAllIndicesLoaded => {
                "after_all_indices_loaded".to_string()
            }
            gix::odb::store::RefreshMode::Never => "never".to_string(),
        }
    }

    /// Write a raw object of the given `kind` (`commit`, `tree`, `blob`, `tag`) through this
    /// pack-aware handle (stored in the first loose object database) and return its id.
    #[napi]
    pub fn write_object(&mut self, kind: String, data: Buffer) -> napi::Result<String> {
        let kind = string_to_kind(&kind)?;
        let id = Write::write_buf(&*self.inner.lock().unwrap(), kind, &data)
            .map_err(|e| err("write_object", e))?;
        Ok(id.to_string())
    }

    /// Like [`write_object`](Self::write_object), but store the object under the given `id` without
    /// recomputing its hash (use only when `id` is the correct hash of `data`).
    #[napi]
    pub fn write_object_with_id(
        &mut self,
        kind: String,
        data: Buffer,
        id: String,
    ) -> napi::Result<String> {
        let kind = string_to_kind(&kind)?;
        let id = parse_oid(&id)?;
        let written = Write::write_buf_with_known_id(&*self.inner.lock().unwrap(), kind, &data, id)
            .map_err(|e| err("write_object_with_id", e))?;
        Ok(written.to_string())
    }

    /// Return the `objects` directory path.
    #[napi(getter)]
    pub fn path(&self) -> String {
        self.path.clone()
    }

    /// Return the hash kind this database uses (`"sha1"`).
    #[napi(getter)]
    pub fn object_hash(&self) -> String {
        self.object_hash.to_string()
    }
}
