use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::error::RepositoryResult as Result;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use gix::objs::{Exists, Find, Write};
use gix::odb::Header as OdbHeader;
use gix::zlib::Compression;

use crate::error::{err, err_str, parse_oid};
use crate::object::{kind_to_string, string_to_kind};

/// The pack-aware, repository-independent object database handle:
/// `gix::odb::store::Handle<Arc<gix::odb::Store>>`.
type HandleStore = gix::odb::store::Handle<Arc<gix::odb::Store>>;
/// The cached object database: `gix::odb::Cache<Handle<Arc<Store>>>`.
type CachedStore = gix::odb::Cache<HandleStore>;

/// Options for constructing a [`ObjectStore`].
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

/// A database for reading and writing loose git objects on disk, one file per object.
///
/// Wraps `gix::odb::loose::Store`. This is the low-level, repository-independent object
/// database: it operates directly on an `objects/` directory and does not consult packs,
/// alternates, or the rest of a repository. For the pack-aware store, see `gix::odb::Store`.
#[napi]
pub struct ObjectStore {
    inner: Backend,
}

/// Backend backing an [`ObjectStore`]. The loose backend writes objects to disk (one file per
/// object); the memory backend keeps them in a `Mutex`-guarded map, acting like git's mempack:
/// write transient objects, then `dump()` them as a pack or drop the whole store to discard.
enum Backend {
    Loose(gix::odb::loose::Store),
    Memory(Arc<InMemoryStore>),
}

/// An in-memory object store, mirroring libgit2's `git_mempack`.
///
/// Objects live in a single `Mutex`-guarded map keyed by their id. This backs
/// `new ObjectStore("memory")`.
struct InMemoryStore {
    objects: Mutex<HashMap<gix::ObjectId, (gix::object::Kind, Vec<u8>)>>,
    kind: gix::hash::Kind,
}

impl InMemoryStore {
    fn new(kind: gix::hash::Kind) -> Self {
        Self {
            objects: Mutex::new(HashMap::new()),
            kind,
        }
    }
    fn contains(&self, id: &str) -> bool {
        gix::ObjectId::from_hex(id.as_bytes())
            .map(|oid| self.objects.lock().unwrap().contains_key(&oid))
            .unwrap_or(false)
    }
    fn try_find(&self, id: &str) -> napi::Result<Option<crate::object::ObjectData>> {
        let oid = gix::ObjectId::from_hex(id.as_bytes()).map_err(|e| err("parse_oid", e))?;
        let map = self.objects.lock().unwrap();
        Ok(map.get(&oid).map(|(kind, data)| crate::object::ObjectData {
            kind: kind_to_string(*kind),
            data: Buffer::from(data.clone()),
        }))
    }
    fn try_header(&self, id: &str) -> napi::Result<Option<crate::object::ObjectHeader>> {
        let oid = gix::ObjectId::from_hex(id.as_bytes()).map_err(|e| err("parse_oid", e))?;
        let map = self.objects.lock().unwrap();
        Ok(map
            .get(&oid)
            .map(|(kind, data)| crate::object::ObjectHeader {
                kind: kind_to_string(*kind),
                size: data.len() as f64,
                is_packed: false,
                num_deltas: None,
            }))
    }
    fn lookup_prefix(&self, prefix: &str) -> napi::Result<PrefixLookup> {
        let p = prefix.to_ascii_lowercase();
        let mut found: Option<gix::ObjectId> = None;
        let mut count = 0usize;
        for oid in self.objects.lock().unwrap().keys() {
            if oid.to_string().starts_with(&p) {
                count += 1;
                found = Some(*oid);
            }
        }
        Ok(match found {
            Some(oid) if count == 1 => PrefixLookup {
                found: true,
                ambiguous: false,
                id: Some(oid.to_string()),
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
    fn reader(&self, id: &str) -> napi::Result<ObjectStoreReader> {
        let oid = gix::ObjectId::from_hex(id.as_bytes()).map_err(|e| err("parse_oid", e))?;
        let map = self.objects.lock().unwrap();
        match map.get(&oid) {
            Some((kind, data)) => Ok(ObjectStoreReader {
                data: data.clone(),
                kind: kind_to_string(*kind),
                pos: 0,
            }),
            None => Err(err_str("reader", format!("object {id} not found"))),
        }
    }
    fn write_object(&self, kind: gix::object::Kind, data: &[u8]) -> napi::Result<gix::ObjectId> {
        let mut hasher = gix::hash::hasher(self.kind);
        let header = format!("{} {}\0", kind_to_string(kind), data.len());
        hasher.update(header.as_bytes());
        hasher.update(data);
        let id = hasher.try_finalize().map_err(|e| err("hash", e))?;
        self.objects
            .lock()
            .unwrap()
            .insert(id, (kind, data.to_vec()));
        Ok(id)
    }
    #[allow(dead_code)]
    fn write_object_with_id(
        &self,
        id: gix::ObjectId,
        kind: gix::object::Kind,
        data: &[u8],
    ) -> napi::Result<()> {
        self.objects
            .lock()
            .unwrap()
            .insert(id, (kind, data.to_vec()));
        Ok(())
    }
}

#[napi]
impl ObjectStore {
    /// Open the loose object database rooted at `objects_directory` (typically `<repo>/.git/objects`).
    ///
    /// `object_hash` is `"sha1"` (default) or `"sha256"`. `options` configures the allocation
    /// limit and compression level used when reading/writing loose objects.
    #[napi(constructor)]
    pub fn new(
        target: String,
        object_hash: Option<String>,
        options: Option<ObjectStoreOptions>,
    ) -> napi::Result<Self> {
        let kind = match object_hash.as_deref() {
            None | Some("sha1") => gix::hash::Kind::Sha1,
            Some(other) => match crate::object::parse_hash_kind_ci(other) {
                Some(kind) => kind,
                None => {
                    return Err(err_str(
                        "ObjectStore::new",
                        format!("unsupported object hash {other} (expected sha1 or sha256)"),
                    ));
                }
            },
        };
        let inner = if target == "memory" {
            Backend::Memory(Arc::new(InMemoryStore::new(kind)))
        } else {
            let options = options.unwrap_or(ObjectStoreOptions {
                alloc_limit_bytes: None,
                compression_level: None,
            });
            let compression = match options.compression_level {
                Some(level) => Compression::new(level as i32).unwrap_or(Compression::BEST_SPEED),
                None => Compression::BEST_SPEED,
            };
            let store = gix::odb::loose::Store::at_opts(
                target,
                kind,
                gix::odb::loose::Options {
                    alloc_limit_bytes: options.alloc_limit_bytes.map(|b| b as usize),
                    compression,
                },
            );
            Backend::Loose(store)
        };
        Ok(ObjectStore { inner })
    }

    /// Whether an object with the given id exists as a loose object.
    #[napi]
    pub fn contains(&self, id: String) -> bool {
        match &self.inner {
            Backend::Loose(inner) => match parse_oid(&id) {
                Ok(oid) => inner.contains(&oid),
                Err(_) => false,
            },
            Backend::Memory(m) => m.contains(&id),
        }
    }

    /// Resolve a short object id `prefix` (hex) to a unique object.
    ///
    /// Returns `found: false` if no object matches, `ambiguous: true` if more than one matches,
    /// and `id` set when exactly one matches.
    #[napi]
    pub fn lookup_prefix(&self, prefix: String) -> napi::Result<PrefixLookup> {
        match &self.inner {
            Backend::Loose(inner) => {
                let prefix =
                    gix::hash::Prefix::from_hex(&prefix).map_err(|e| err("lookup_prefix", e))?;
                match inner.lookup_prefix(prefix, None) {
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
            Backend::Memory(m) => m.lookup_prefix(&prefix),
        }
    }

    /// Read an object's raw, decompressed body (without the loose-object header) and kind.
    /// Returns `null` if the object is missing.
    #[napi]
    pub fn try_find(&self, id: String) -> napi::Result<Option<crate::object::ObjectData>> {
        match &self.inner {
            Backend::Loose(inner) => {
                let oid = parse_oid(&id)?;
                let mut buf = Vec::new();
                let data = inner
                    .try_find(&oid, &mut buf)
                    .map_err(|e| err("try_find", e))?;
                Ok(data.map(|d| crate::object::ObjectData {
                    kind: kind_to_string(d.kind),
                    data: Buffer::from(d.data.to_vec()),
                }))
            }
            Backend::Memory(m) => m.try_find(&id),
        }
    }

    /// Read an object's header (kind + size) without decoding its full content.
    /// Returns `null` if the object is missing.
    #[napi]
    pub fn try_header(&self, id: String) -> napi::Result<Option<crate::object::ObjectHeader>> {
        match &self.inner {
            Backend::Loose(inner) => {
                let oid = parse_oid(&id)?;
                let header = inner.try_header(&oid).map_err(|e| err("try_header", e))?;
                Ok(header.map(|(size, kind)| crate::object::ObjectHeader {
                    kind: kind_to_string(kind),
                    size: size as f64,
                    is_packed: false,
                    num_deltas: None,
                }))
            }
            Backend::Memory(m) => m.try_header(&id),
        }
    }

    /// Write a raw object of the given `kind` (`commit`, `tree`, `blob`, `tag`) and return its id.
    #[napi]
    pub fn write_object(&self, kind: String, data: Buffer) -> napi::Result<String> {
        let kind = string_to_kind(&kind)?;
        let id = match &self.inner {
            Backend::Loose(inner) => inner
                .write_buf(kind, &data)
                .map_err(|e| err("write_object", e))?,
            Backend::Memory(m) => m.write_object(kind, &data)?,
        };
        Ok(id.to_string())
    }

    /// Return the absolute path of the loose object file for `id` (may not exist yet).
    #[napi]
    pub fn object_path(&self, id: String) -> napi::Result<String> {
        match &self.inner {
            Backend::Loose(inner) => {
                let oid = parse_oid(&id)?;
                Ok(inner.object_path(&oid).to_string_lossy().into_owned())
            }
            Backend::Memory(_) => Err(err_str(
                "object_path",
                "in-memory object stores have no on-disk path",
            )),
        }
    }

    /// Return the `objects` directory path.
    #[napi(getter)]
    pub fn path(&self) -> String {
        match &self.inner {
            Backend::Loose(inner) => inner.path().to_string_lossy().into_owned(),
            Backend::Memory(_) => "memory".to_string(),
        }
    }

    /// Return the hash kind this store uses (`"sha1"` or `"sha256"`).
    #[napi(getter)]
    pub fn object_hash(&self) -> String {
        match &self.inner {
            Backend::Loose(inner) => inner.object_hash().to_string(),
            Backend::Memory(m) => m.kind.to_string(),
        }
    }

    /// Open a streaming reader for the object identified by `id`.
    ///
    /// The returned [`ObjectStoreReader`] exposes the object's `kind` and `size` and yields the
    /// raw, decompressed content in chunks via `read`. The loose object is read in full up front
    /// (gix does not offer incremental on-disk streaming for loose objects), but the bytes are
    /// delivered to the caller incrementally.
    #[napi]
    pub fn reader(&self, id: String) -> napi::Result<ObjectStoreReader> {
        match &self.inner {
            Backend::Loose(inner) => {
                let oid = parse_oid(&id)?;
                let mut buf = Vec::new();
                let data = inner
                    .try_find(&oid, &mut buf)
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
            Backend::Memory(m) => m.reader(&id),
        }
    }

    /// Open a streaming writer for a new object of the given `kind` (`commit`, `tree`, `blob`, `tag`)
    /// and total `size` in bytes.
    ///
    /// Push content with `write` and call `finalize` to store the object and obtain its id. Content
    /// is buffered and the object is written on `finalize`; the total written must equal the declared
    /// `size` (pass `0`/`null` to skip the size check).
    #[napi]
    pub fn writer(&self, kind: String, size: f64) -> napi::Result<ObjectStoreWriter> {
        let kind = string_to_kind(&kind)?;
        let target = match &self.inner {
            Backend::Loose(inner) => WriterTarget::Loose(Mutex::new(inner.clone())),
            Backend::Memory(m) => WriterTarget::Memory(m.clone()),
        };
        Ok(ObjectStoreWriter {
            target,
            kind,
            buf: Vec::with_capacity(size as usize),
            expected_size: if size <= 0.0 {
                None
            } else {
                Some(size as usize)
            },
        })
    }

    fn memory_arc(&self) -> napi::Result<Arc<InMemoryStore>> {
        match &self.inner {
            Backend::Memory(m) => Ok(m.clone()),
            _ => Err(err_str(
                "ObjectStore",
                "this operation requires an in-memory object store (new ObjectStore(\"memory\"))",
            )),
        }
    }

    /// Serialize every object currently held in memory into a git packfile `Buffer`.
    ///
    /// Only valid for in-memory stores (`new ObjectStore("memory")`). The produced pack can be
    /// restored via [`load`](Self::load) or fed to `git unpack-objects`.
    #[napi]
    pub fn dump(&self) -> napi::Result<Buffer> {
        let arc = self.memory_arc()?;
        let kind = arc.kind;
        let mut entries = Vec::new();
        {
            let map = arc.objects.lock().unwrap();
            for (id, (k, data)) in map.iter() {
                let count = gix_pack::data::output::Count {
                    id: *id,
                    entry_pack_location: gix_pack::data::output::count::PackLocation::NotLookedUp,
                };
                let obj = gix::objs::Data {
                    kind: *k,
                    object_hash: kind,
                    data: data.as_slice(),
                };
                let entry =
                    gix_pack::data::output::Entry::from_data(&count, &obj, Compression::DEFAULT)
                        .map_err(|e| err("dump", e))?;
                entries.push(entry);
            }
        }
        let num = entries.len() as u32;
        let mut out = Vec::new();
        let iter = std::iter::once(Ok(entries));
        let mut writer = gix_pack::data::output::bytes::FromEntriesIter::new(
            iter,
            &mut out,
            num,
            gix_pack::data::Version::V2,
            kind,
        );
        while let Some(res) = writer.next() {
            res.map_err(|e| err("dump", e))?;
        }
        Ok(Buffer::from(out))
    }

    /// Load a packfile `Buffer` (e.g. one produced by [`dump`](Self::dump)) into the in-memory
    /// store, adding all of its objects. Objects whose id already exists are overwritten.
    ///
    /// Only valid for in-memory stores.
    #[napi]
    pub fn load(&self, pack: Buffer) -> napi::Result<()> {
        let arc = self.memory_arc()?;
        let kind = arc.kind;
        let dir = std::env::temp_dir().join(format!("repo-mem-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).map_err(|e| err("load", e))?;
        let pack_path = dir.join("tmp.pack");
        std::fs::write(&pack_path, &pack).map_err(|e| err("load", e))?;
        let file = gix_pack::data::File::at(&pack_path, kind).map_err(|e| err("load", e))?;
        let mut inflate = gix::zlib::Inflate {
            state: gix::zlib::Decompress::new(),
        };
        let mut cache = gix_pack::cache::Never;
        let mut offset = gix_pack::data::header::SIZE as u64;
        while offset < file.pack_end() as u64 {
            let entry = file.entry(offset).map_err(|e| err("load", e))?;
            let header_size = entry.header_size();
            let mut out = Vec::new();
            let outcome = file
                .decode_entry(
                    entry,
                    &mut out,
                    &mut inflate,
                    &resolve_pack_base,
                    &mut cache,
                )
                .map_err(|e| err("load", e))?;
            let mut h = gix::hash::hasher(kind);
            let header = format!("{} {}\0", kind_to_string(outcome.kind), out.len());
            h.update(header.as_bytes());
            h.update(&out);
            let id = h.try_finalize().map_err(|e| err("load", e))?;
            arc.objects.lock().unwrap().insert(id, (outcome.kind, out));
            offset += header_size as u64 + outcome.compressed_size as u64;
        }
        Ok(())
    }

    /// Remove every object currently held in memory, leaving the store empty.
    ///
    /// Only valid for in-memory stores (`new ObjectStore("memory")`). Mirrors libgit2's
    /// `git_mempack_reset`.
    #[napi]
    pub fn reset(&self) -> napi::Result<()> {
        let arc = self.memory_arc()?;
        arc.objects.lock().unwrap().clear();
        Ok(())
    }

    /// Return the hex ids of every object currently held in memory, in insertion order.
    ///
    /// Only valid for in-memory stores.
    #[napi]
    pub fn entries(&self) -> napi::Result<Vec<String>> {
        let arc = self.memory_arc()?;
        let map = arc.objects.lock().unwrap();
        Ok(map.keys().map(|id| id.to_string()).collect())
    }

    /// Return the number of objects currently held in memory.
    ///
    /// Only valid for in-memory stores.
    #[napi]
    pub fn size(&self) -> napi::Result<f64> {
        let arc = self.memory_arc()?;
        Ok(arc.objects.lock().unwrap().len() as f64)
    }

    /// Write a raw object of the given `kind` under the exact `id`, without recomputing its hash.
    /// Use only when `id` is the correct hash of `data`; a mismatch is not validated here.
    #[napi]
    pub fn write_object_with_id(
        &self,
        kind: String,
        data: Buffer,
        id: String,
    ) -> napi::Result<String> {
        let kind = string_to_kind(&kind)?;
        let oid = parse_oid(&id)?;
        match &self.inner {
            Backend::Loose(inner) => {
                let written = gix::objs::Write::write_buf_with_known_id(&*inner, kind, &data, oid)
                    .map_err(|e| err("write_object_with_id", e))?;
                Ok(written.to_string())
            }
            Backend::Memory(m) => {
                m.write_object_with_id(oid, kind, &data)?;
                Ok(id)
            }
        }
    }
}

/// Resolver used when decoding a packfile without any base objects available
/// (the in-memory store only ever holds fully-resolved objects, so deltas
/// against external bases are not supported here).
fn resolve_pack_base(
    _oid: &gix::hash::oid,
    _out: &mut Vec<u8>,
) -> std::result::Result<Option<gix_pack::data::decode::entry::ResolvedBase>, gix::Error> {
    Ok(None)
}

/// The target a [`ObjectStoreWriter`] flushes its buffered object into.
enum WriterTarget {
    Loose(Mutex<gix::odb::loose::Store>),
    Handle(Mutex<HandleStore>),
    Memory(Arc<InMemoryStore>),
}

/// A streaming reader over an object's raw, decompressed content.
///
/// Returned by [`ObjectStore::reader`], [`ObjectHandle::reader`] and [`ObjectDatabase::reader`].
/// The object is loaded in full on creation; [`read`](Self::read) hands it back in chunks.
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
/// Returned by [`ObjectStore::writer`], [`ObjectHandle::writer`] and [`ObjectDatabase::writer`].
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
        };
        Ok(id.to_string())
    }
}

/// Options for constructing an [`ObjectDatabase`].
#[napi(object)]
pub struct ObjectDatabaseOptions {
    /// Object hash kind: `"sha1"` (default). SHA-256 requires the gix `sha256` feature.
    pub object_hash: Option<String>,
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

/// A pack-aware, repository-independent object database wrapped in gix's caching layer.
///
/// Wraps `gix::odb::Cache<Handle<Arc<gix::odb::Store>>>`. Unlike [`ObjectStore`], this consults
/// packs and alternates in addition to loose objects, and supports pack/object caches to speed up
/// repeated or delta-chain decodes. Use [`ObjectDatabase::into_inner`] to drop the cache and obtain
/// the bare [`ObjectHandle`].
///
/// The wrapped `Cache` is not `Sync` (it holds `RefCell`s), so it is kept behind a `Mutex` here.
#[napi]
pub struct ObjectDatabase {
    inner: Mutex<CachedStore>,
    path: String,
    object_hash: gix::hash::Kind,
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
            &mut std::iter::empty::<(gix::hash::ObjectId, gix::hash::ObjectId)>(),
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
        })
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
