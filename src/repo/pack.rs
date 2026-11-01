use std::collections::HashSet;
use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::error::RepositoryResult as Result;
use napi::Task;
use napi::bindgen_prelude::AsyncTask;
use napi::bindgen_prelude::Buffer;
use napi::bindgen_prelude::Env;
use napi::bindgen_prelude::Function;
use napi::threadsafe_function::ThreadsafeFunction;
use napi_derive::napi;

use gix::zlib::Compression;
use gix_pack::Bundle;
use gix_pack::Find;
use gix_pack::bundle::write::Options as BundleWriteOptions;
use gix_pack::data::Version;
use gix_pack::data::entry::Header as PackHeader;
use gix_pack::data::input::Mode as InputMode;
use gix_pack::data::output::Count as PackCount;
use gix_pack::data::output::Entry as PackEntryData;
use gix_pack::data::output::bytes::FromEntriesIter;
use gix_pack::data::output::count::objects as count_objects;
use gix_pack::data::output::count::objects::ObjectExpansion;
use gix_pack::data::output::count::objects::Options as CountOptions;
use gix_pack::data::output::entry::iter_from_counts;
use gix_pack::data::output::entry::iter_from_counts::Mode as EntryMode;
use gix_pack::data::output::entry::iter_from_counts::Options as EntryOptions;

use crate::error::err_str;
use crate::error::{err, parse_oid, thread_local};
use crate::process::CancellationToken;
use crate::process::cancellation_flag;
use crate::progress::ProgressCallback;
use crate::progress::ProgressEvent;
use crate::repository::Repository;
use crate::rev_walk::WalkOptions;

/// A single entry (full or delta object) within a pack data file, mirroring `gix_pack::data::Entry`.
///
/// Decode one with [`PackEntry::from_bytes`], then read its [`PackEntry::header`], sizes and offsets.
#[napi]
pub struct PackEntry {
    inner: gix_pack::data::Entry,
}

/// Map `"sha1"` / `"sha256"` to the corresponding `gix::hash::Kind`.
///
/// Note: `sha256` requires the `sha256` feature on `gix`, which is not enabled in this build, so
/// it is rejected at runtime rather than at compile time.
fn hash_kind(s: &str) -> napi::Result<gix::hash::Kind> {
    match s.to_ascii_lowercase().as_str() {
        "sha1" => Ok(gix::hash::Kind::Sha1),
        "sha256" => Err(err_str(
            "pack_entry_hash",
            "sha256 is not enabled in this build; add the 'sha256' feature to the 'gix' dependency",
        )),
        other => Err(err_str(
            "pack_entry_hash",
            format!("unsupported object hash '{other}', expected 'sha1' or 'sha256'"),
        )),
    }
}

#[napi]
impl PackEntry {
    /// Decode a pack entry from the start of `data` (the bytes of a pack data file, or a slice
    /// beginning at `packOffset`). `packOffset` is the absolute byte offset of this entry within the
    /// pack, and `hash` is `"sha1"` or `"sha256"`.
    #[napi]
    pub fn from_bytes(data: Buffer, pack_offset: f64, hash: String) -> napi::Result<PackEntry> {
        let pack_offset = u64::try_from(pack_offset as i64).map_err(|_| {
            err_str(
                "pack_entry_from_bytes",
                "pack_offset must be a non-negative integer",
            )
        })?;
        let kind = hash_kind(&hash)?;
        let entry = gix_pack::data::Entry::from_bytes(&data, pack_offset, kind)
            .map_err(|e| err("pack_entry_from_bytes", e))?;
        Ok(PackEntry { inner: entry })
    }

    /// The header of this entry, describing the object kind and (for deltas) the base.
    #[napi(getter)]
    pub fn header(&self) -> PackEntryHeader {
        PackEntryHeader {
            inner: self.inner.header,
        }
    }

    /// The decompressed size of the entry in bytes.
    #[napi(getter)]
    pub fn decompressed_size(&self) -> f64 {
        self.inner.decompressed_size as f64
    }

    /// Absolute offset to the compressed object data, just behind the entry's header.
    #[napi(getter)]
    pub fn data_offset(&self) -> f64 {
        self.inner.data_offset as f64
    }

    /// Number of bytes used to encode the entry header in the pack (preserves non-canonical encodings).
    #[napi(getter)]
    pub fn encoded_header_size(&self) -> u32 {
        self.inner.encoded_header_size as u32
    }

    /// The size, in bytes, of the header as actually present in the pack. Unlike `Header`-based
    /// canonical sizing, this preserves overlong (non-canonical) size encodings.
    #[napi]
    pub fn header_size(&self) -> f64 {
        self.inner.header_size() as f64
    }

    /// The absolute offset of the entry's start in the pack, computed from `data_offset` and the
    /// actually-encoded header size.
    #[napi]
    pub fn pack_offset(&self) -> f64 {
        self.inner.pack_offset() as f64
    }

    /// Given an ofs-delta `distance`, return the absolute pack offset of the base entry, or `null`
    /// if `distance` is zero or would point before the pack header.
    #[napi]
    pub fn checked_base_pack_offset(&self, distance: f64) -> Option<f64> {
        let distance = u64::try_from(distance as i64).ok()?;
        self.inner
            .checked_base_pack_offset(distance)
            .map(|o| o as f64)
    }
}

/// The header portion of a pack entry: the kind of stored object, or (for deltas) how the base is
/// identified. Mirrors `gix_pack::data::entry::Header`.
#[napi]
pub struct PackEntryHeader {
    inner: PackHeader,
}

#[napi]
impl PackEntryHeader {
    /// The entry kind: `"blob"`, `"tree"`, `"commit"`, `"tag"`, `"ofs_delta"`, or `"ref_delta"`.
    #[napi(getter)]
    pub fn kind(&self) -> String {
        match self.inner {
            PackHeader::Blob => "blob",
            PackHeader::Tree => "tree",
            PackHeader::Commit => "commit",
            PackHeader::Tag => "tag",
            PackHeader::OfsDelta { .. } => "ofs_delta",
            PackHeader::RefDelta { .. } => "ref_delta",
        }
        .to_string()
    }

    /// `true` if this is a delta object (ofs- or ref-delta).
    #[napi(getter)]
    pub fn is_delta(&self) -> bool {
        self.inner.is_delta()
    }

    /// `true` if this is a base object (not a delta).
    #[napi(getter)]
    pub fn is_base(&self) -> bool {
        self.inner.is_base()
    }

    /// The object kind as a git object type (`"blob"`/`"tree"`/`"commit"`/`"tag"`), or `null` for
    /// delta entries which have no concrete object kind.
    #[napi(getter)]
    pub fn object_kind(&self) -> Option<String> {
        self.inner.as_kind().map(|k| {
            match k {
                gix::object::Kind::Blob => "blob",
                gix::object::Kind::Tree => "tree",
                gix::object::Kind::Commit => "commit",
                gix::object::Kind::Tag => "tag",
            }
            .to_string()
        })
    }

    /// For `ref_delta` entries, the hex object id of the base; `null` otherwise.
    #[napi(getter)]
    pub fn base_id(&self) -> Option<String> {
        match self.inner {
            PackHeader::RefDelta { ref base_id } => Some(base_id.to_string()),
            _ => None,
        }
    }

    /// For `ofs_delta` entries, the base distance (relative to this entry's pack offset); `null` otherwise.
    #[napi(getter)]
    pub fn base_distance(&self) -> Option<i64> {
        match self.inner {
            PackHeader::OfsDelta { base_distance } => Some(base_distance as i64),
            _ => None,
        }
    }

    /// The pack-internal type id byte for this header (e.g. `1` for commit, `3` for blob).
    #[napi(getter)]
    pub fn type_id(&self) -> f64 {
        self.inner.as_type_id() as f64
    }
}

// ---------------------------------------------------------------------------
// Pack building
// ---------------------------------------------------------------------------

/// Options for [`Pack::build`].
#[napi(object)]
pub struct PackBuildOptions {
    /// Maximum number of threads to use while counting and generating pack entries.
    /// `None` (the default) lets the library use all available cores.
    pub threads: Option<u32>,
    /// zlib compression level (0–9) for base objects added to the pack. Defaults to 6.
    pub compression_level: Option<u32>,
}

/// Options shared by the terminal [`Pack::to_buffer`] and [`Pack::write_to_directory`] operations.
///
/// The progress callback is passed positionally (like `Repository.clone` and `Remote.fetch`)
/// because napi `Function` values carry a lifetime and cannot be stored in an options object.
#[napi(object)]
pub struct PackOperationOptions {
    /// Optional cancellation token; requesting cancellation aborts the operation at the
    /// next interrupt point.
    pub cancellation: Option<CancellationToken>,
}

/// Builder that accumulates a plan of objects to include in a packfile, then materialises the
/// pack asynchronously.
///
/// Create one with the static factory [`Pack::build`]:
///
/// ```js
/// const pack = Pack.build(repo);
/// pack.addCommit(commitId);
/// const { data, packHash } = await pack.toBuffer();
/// ```
///
/// Insertion methods are synchronous and only record intent; the heavy counting, compression
/// and (optionally) indexing work happens in the terminal async call.
#[napi]
pub struct Pack {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) threads: Option<usize>,
    pub(crate) compression: Compression,
    pub(crate) asis: Vec<gix::ObjectId>,
    pub(crate) expand: Vec<gix::ObjectId>,
}

/// Result of writing a packfile into an in-memory buffer.
#[napi(object)]
pub struct PackBufferResult {
    /// The serialised packfile bytes.
    pub data: Buffer,
    /// The hex object id derived from the packfile name (its trailing hash).
    pub pack_hash: String,
    /// The number of objects written to the pack.
    pub object_count: f64,
    /// The number of bytes written (the full packfile size).
    pub bytes_written: f64,
}

/// Result of writing a packfile and its index into a directory.
#[napi(object)]
pub struct PackWriteResult {
    /// The hex object id derived from the packfile name.
    pub pack_hash: String,
    /// The hex object id of the generated `.idx` file.
    pub index_hash: String,
    /// The number of objects written to the pack.
    pub object_count: f64,
    /// The number of bytes written for the packfile.
    pub bytes_written: f64,
    /// Absolute path to the written `.pack` file, or `null` for an empty pack.
    pub pack_path: Option<String>,
    /// Absolute path to the written `.idx` file, or `null` for an empty pack.
    pub index_path: Option<String>,
    /// Absolute path to a `.keep` file if one was created, otherwise `null`.
    pub keep_path: Option<String>,
}

/// `gix_pack::Find` adapter over a shareable object database handle.
///
/// `gix::OdbHandleArc` derefs to a `gix_pack::Find` implementation, but the trait is not
/// auto-forwarded through `Deref`, so each method is forwarded explicitly. The handle is
/// `Send + Clone + 'static`, satisfying the bounds required by the parallel pack pipeline.
#[derive(Clone)]
struct OdbFind(gix::OdbHandleArc);

impl Find for OdbFind {
    fn contains(&self, id: &gix::hash::oid) -> bool {
        Deref::deref(&self.0).contains(id)
    }

    fn try_find_cached<'a>(
        &self,
        id: &gix::hash::oid,
        buffer: &'a mut Vec<u8>,
        pack_cache: &mut dyn gix_pack::cache::DecodeEntry,
    ) -> std::result::Result<
        Option<(gix::objs::Data<'a>, Option<gix_pack::data::entry::Location>)>,
        gix::error::Error,
    > {
        Deref::deref(&self.0).try_find_cached(id, buffer, pack_cache)
    }

    fn location_by_oid(
        &self,
        id: &gix::hash::oid,
        buf: &mut Vec<u8>,
    ) -> std::result::Result<Option<gix_pack::data::entry::Location>, gix::error::Error> {
        Deref::deref(&self.0).location_by_oid(id, buf)
    }

    fn pack_offsets_and_oid(
        &self,
        pack_id: u32,
    ) -> std::result::Result<
        Option<Vec<(gix_pack::data::Offset, gix::hash::ObjectId)>>,
        gix::error::Error,
    > {
        Deref::deref(&self.0).pack_offsets_and_oid(pack_id)
    }

    fn entry_by_location(
        &self,
        location: &gix_pack::data::entry::Location,
    ) -> Option<gix_pack::find::Entry> {
        Deref::deref(&self.0).entry_by_location(location)
    }
}

/// Build the packfile bytes for the planned objects.
///
/// Returns the serialised packfile, its name hash, and the number of objects written.
/// An empty plan produces the canonical empty packfile.
fn build_pack_bytes(
    repo: &Arc<gix::ThreadSafeRepository>,
    asis: &[gix::ObjectId],
    expand: &[gix::ObjectId],
    threads: Option<usize>,
    compression: Compression,
    progress: &ProgressCallback,
    interrupt: &AtomicBool,
) -> napi::Result<(Vec<u8>, gix::ObjectId, usize)> {
    let r = thread_local(repo);
    let object_hash = r.object_hash();
    let mut odb: gix::OdbHandleArc = r
        .objects
        .clone()
        .into_arc()
        .map_err(|e| err("pack_objects", e))?;
    // The pipeline stores pack ids during counting and resolves them later while entries are
    // produced in parallel; packs must therefore stay mapped instead of being unloaded.
    (&mut *odb).prevent_pack_unload();
    let find = OdbFind(odb);

    // Count phase: run once per expansion mode and merge, de-duplicating by object id.
    let mut merged: Vec<PackCount> = Vec::new();
    let mut seen: HashSet<gix::ObjectId> = HashSet::new();
    for (ids, expansion) in [
        (asis, ObjectExpansion::AsIs),
        (expand, ObjectExpansion::TreeContents),
    ] {
        if ids.is_empty() {
            continue;
        }
        // Own the ids so the boxed iterator is `'static` as the counting API requires.
        let iter = ids.to_vec().into_iter().map(Ok);
        let (counts, _outcome) = count_objects(
            find.clone(),
            Box::new(iter),
            &*progress,
            interrupt,
            CountOptions {
                thread_limit: threads,
                input_object_expansion: expansion,
                chunk_size: 10,
            },
        )
        .map_err(|e| err("pack_count", e))?;
        for c in counts {
            if seen.insert(c.id) {
                merged.push(c);
            }
        }
    }

    let object_count = merged.len();
    if object_count == 0 {
        let empty: Vec<Vec<PackEntryData>> = Vec::new();
        let mut out = Vec::new();
        let mut writer = FromEntriesIter::new(
            empty.into_iter().map(Ok),
            &mut out,
            0,
            Version::default(),
            object_hash,
        );
        while writer.next().is_some() {}
        let pack_hash = writer
            .digest()
            .expect("empty pack always yields a trailing hash");
        return Ok((out, pack_hash, 0));
    }

    let entry_opts = EntryOptions {
        version: Version::default(),
        mode: EntryMode::PackCopyAndBaseObjects,
        allow_thin_pack: false,
        thread_limit: threads,
        chunk_size: 10,
        compression,
    };
    let iter = iter_from_counts(
        merged.clone(),
        find.clone(),
        Box::new(progress.clone()),
        entry_opts,
    )
    .map_err(|e| err("pack_entries", e))?;
    let mut chunks = iter
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| err("pack_entries", e))?;
    // Entries must be written in sequence-id order so ofs-delta base offsets stay valid.
    chunks.sort_by(|a, b| a.0.cmp(&b.0));
    let entries: Vec<Vec<PackEntryData>> = chunks.into_iter().map(|(_, e)| e).collect();

    let mut out = Vec::new();
    let mut writer = FromEntriesIter::new(
        entries.into_iter().map(Ok),
        &mut out,
        object_count as u32,
        Version::default(),
        object_hash,
    );
    while writer.next().is_some() {}
    let pack_hash = writer
        .digest()
        .expect("pack always yields a trailing hash after writing");
    Ok((out, pack_hash, object_count))
}

/// Resolve the interrupt flag from the optional cancellation token (or a fresh one).
fn interrupt_for(cancellation: &Option<CancellationToken>) -> Arc<AtomicBool> {
    cancellation
        .as_ref()
        .and_then(cancellation_flag)
        .unwrap_or_else(|| Arc::new(AtomicBool::new(false)))
}

/// Pull the progress callback and cancellation token out of the operation arguments.
fn extract(
    on_progress: Option<Function<ProgressEvent, ()>>,
    options: Option<PackOperationOptions>,
) -> (
    Option<Arc<ThreadsafeFunction<ProgressEvent, ()>>>,
    Option<CancellationToken>,
) {
    let progress = match on_progress {
        Some(f) => f
            .build_threadsafe_function::<ProgressEvent>()
            .callee_handled()
            .build()
            .ok()
            .map(Arc::new),
        None => None,
    };
    let cancellation = options.and_then(|o| o.cancellation);
    (progress, cancellation)
}

#[napi]
impl Pack {
    /// Create a pack builder for `repository`.
    ///
    /// ```js
    /// const pack = Pack.build(repo, { threads: 4, compressionLevel: 9 });
    /// ```
    #[napi(factory)]
    pub fn build(repository: &Repository, options: Option<PackBuildOptions>) -> napi::Result<Pack> {
        let (threads, compression) = match options {
            Some(o) => (
                o.threads.map(|t| t as usize),
                o.compression_level
                    .map(|l| Compression::new(l as i32).unwrap_or(Compression::DEFAULT))
                    .unwrap_or(Compression::DEFAULT),
            ),
            None => (None, Compression::DEFAULT),
        };
        Ok(Pack {
            repo: Arc::clone(&repository.inner),
            threads,
            compression,
            asis: Vec::new(),
            expand: Vec::new(),
        })
    }

    /// Add a single object (blob, tree, commit or tag) as-is, without expanding its children.
    #[napi]
    pub fn add_object(&mut self, id: String) -> napi::Result<()> {
        self.asis.push(parse_oid(&id)?);
        Ok(())
    }

    /// Add several objects as-is, without expanding their children.
    #[napi]
    pub fn add_objects(&mut self, ids: Vec<String>) -> napi::Result<()> {
        for id in ids {
            self.asis.push(parse_oid(&id)?);
        }
        Ok(())
    }

    /// Add a tree and all of the objects it references (nested trees and blobs).
    #[napi]
    pub fn add_tree(&mut self, id: String) -> napi::Result<()> {
        self.expand.push(parse_oid(&id)?);
        Ok(())
    }

    /// Add a commit and the complete tree it references.
    #[napi]
    pub fn add_commit(&mut self, id: String) -> napi::Result<()> {
        self.expand.push(parse_oid(&id)?);
        Ok(())
    }

    /// Resolve the given tips with the supplied walk options and add each reachable commit
    /// together with the tree it references.
    #[napi]
    pub fn add_rev_walk(
        &mut self,
        tips: Vec<String>,
        options: Option<WalkOptions>,
    ) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        let tip_ids: Vec<gix::ObjectId> = tips
            .iter()
            .map(|t| parse_oid(t))
            .collect::<Result<Vec<_>>>()?;
        let mut platform = r.rev_walk(tip_ids);
        if let Some(opts) = options {
            if opts.first_parent.unwrap_or(false) {
                platform = platform.first_parent_only();
            }
            if let Some(sort) = opts.sort.as_deref() {
                let sorting = match sort {
                    "time-newest" => gix::revision::walk::Sorting::ByCommitTime(
                        gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
                    ),
                    "time-oldest" => gix::revision::walk::Sorting::ByCommitTime(
                        gix::traverse::commit::simple::CommitTimeOrder::OldestFirst,
                    ),
                    _ => gix::revision::walk::Sorting::BreadthFirst,
                };
                platform = platform.sorting(sorting);
            }
        }
        let ids: Vec<String> = platform
            .all()
            .map_err(|e| err("rev_walk", e))?
            .map(|i| i.map(|i| i.id.to_string()))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| err("rev_walk_item", e))?;
        for id in ids {
            self.expand.push(parse_oid(&id)?);
        }
        Ok(())
    }

    /// Clear the accumulated plan so the builder can be reused.
    #[napi]
    pub fn clear(&mut self) {
        self.asis.clear();
        self.expand.clear();
    }

    /// Asynchronously generate the packfile into an in-memory buffer.
    #[napi(ts_return_type = "Promise<PackBufferResult>")]
    pub fn to_buffer(
        &self,
        _env: Env,
        on_progress: Option<Function<ProgressEvent, ()>>,
        options: Option<PackOperationOptions>,
    ) -> napi::Result<AsyncTask<PackBufferTask>> {
        let (progress, cancellation) = extract(on_progress, options);
        Ok(AsyncTask::new(PackBufferTask {
            repo: Arc::clone(&self.repo),
            threads: self.threads,
            compression: self.compression,
            asis: self.asis.clone(),
            expand: self.expand.clone(),
            progress,
            cancellation,
        }))
    }

    /// Asynchronously generate the packfile and write it (with its index) into `directory`.
    #[napi(ts_return_type = "Promise<PackWriteResult>")]
    pub fn write_to_directory(
        &self,
        directory: String,
        _env: Env,
        on_progress: Option<Function<ProgressEvent, ()>>,
        options: Option<PackOperationOptions>,
    ) -> napi::Result<AsyncTask<PackWriteTask>> {
        let (progress, cancellation) = extract(on_progress, options);
        Ok(AsyncTask::new(PackWriteTask {
            repo: Arc::clone(&self.repo),
            threads: self.threads,
            compression: self.compression,
            asis: self.asis.clone(),
            expand: self.expand.clone(),
            directory,
            progress,
            cancellation,
        }))
    }
}

/// Async task that builds the packfile into an in-memory buffer.
pub struct PackBufferTask {
    repo: Arc<gix::ThreadSafeRepository>,
    threads: Option<usize>,
    compression: Compression,
    asis: Vec<gix::ObjectId>,
    expand: Vec<gix::ObjectId>,
    progress: Option<Arc<ThreadsafeFunction<ProgressEvent, ()>>>,
    cancellation: Option<CancellationToken>,
}

impl Task for PackBufferTask {
    type Output = PackBufferResult;
    type JsValue = PackBufferResult;

    fn compute(&mut self) -> napi::Result<PackBufferResult> {
        let interrupt = interrupt_for(&self.cancellation);
        let progress = ProgressCallback::new(self.progress.clone());
        let (bytes, pack_hash, object_count) = build_pack_bytes(
            &self.repo,
            &self.asis,
            &self.expand,
            self.threads,
            self.compression,
            &progress,
            &interrupt,
        )?;
        let bytes_written = bytes.len() as f64;
        Ok(PackBufferResult {
            data: Buffer::from(bytes),
            pack_hash: pack_hash.to_string(),
            object_count: object_count as f64,
            bytes_written,
        })
    }

    fn resolve(&mut self, _env: Env, output: PackBufferResult) -> napi::Result<PackBufferResult> {
        Ok(output)
    }
}

/// Async task that builds the packfile and writes it (with index) to a directory.
pub struct PackWriteTask {
    repo: Arc<gix::ThreadSafeRepository>,
    threads: Option<usize>,
    compression: Compression,
    asis: Vec<gix::ObjectId>,
    expand: Vec<gix::ObjectId>,
    directory: String,
    progress: Option<Arc<ThreadsafeFunction<ProgressEvent, ()>>>,
    cancellation: Option<CancellationToken>,
}

impl Task for PackWriteTask {
    type Output = PackWriteResult;
    type JsValue = PackWriteResult;

    fn compute(&mut self) -> napi::Result<PackWriteResult> {
        let interrupt = interrupt_for(&self.cancellation);
        let progress = ProgressCallback::new(self.progress.clone());
        let (bytes, _pack_hash, object_count) = build_pack_bytes(
            &self.repo,
            &self.asis,
            &self.expand,
            self.threads,
            self.compression,
            &progress,
            &interrupt,
        )?;
        let bytes_written = bytes.len() as f64;
        let object_hash = thread_local(&self.repo).object_hash();

        let mut reader = std::io::BufReader::new(std::io::Cursor::new(bytes));
        let mut write_progress = ProgressCallback::new(self.progress.clone());
        let outcome = Bundle::write_to_directory(
            &mut reader,
            Some(std::path::Path::new(&self.directory)),
            &mut write_progress,
            &interrupt,
            Option::<gix::Repository>::None,
            object_hash,
            BundleWriteOptions {
                thread_limit: self.threads,
                iteration_mode: InputMode::Verify,
                index_version: gix_pack::index::Version::default(),
                alloc_limit_bytes: None,
                compression: self.compression,
            },
        )
        .map_err(|e| err("pack_write", e))?;

        Ok(PackWriteResult {
            pack_hash: outcome.index.data_hash.to_string(),
            index_hash: outcome.index.index_hash.to_string(),
            object_count: object_count as f64,
            bytes_written,
            pack_path: outcome.data_path.map(|p| p.to_string_lossy().into_owned()),
            index_path: outcome.index_path.map(|p| p.to_string_lossy().into_owned()),
            keep_path: outcome.keep_path.map(|p| p.to_string_lossy().into_owned()),
        })
    }

    fn resolve(&mut self, _env: Env, output: PackWriteResult) -> napi::Result<PackWriteResult> {
        Ok(output)
    }
}
