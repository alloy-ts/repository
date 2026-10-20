use napi::Result;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;
use std::io::Write;

use crate::error::{err, err_str, parse_oid, thread_local};
use crate::repository::{IndexEntry, IndexEntryInput, Repository};

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn bstr(s: &str) -> &gix::bstr::BStr {
    gix::bstr::BStr::new(s.as_bytes())
}

/// Filesystem stat information of an index entry, mirroring `gix_index::entry::Stat`.
/// Filesystem stat information of an index entry, mirroring `gix_index::entry::Stat`.
///
/// A plain object (not a class) so it can be embedded in index entry objects and
/// round-trip through JS; use `indexStatIsRacy()`/`indexStatMatches()` for the
/// `gix_index::entry::Stat` comparison helpers.
#[derive(Clone)]
#[napi(object)]
pub struct IndexStat {
    pub ctime_secs: u32,
    pub ctime_nsecs: u32,
    pub mtime_secs: u32,
    pub mtime_nsecs: u32,
    pub dev: u32,
    pub ino: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u32,
}

impl IndexStat {
    pub(crate) fn from_gix(stat: gix::index::entry::Stat) -> Self {
        IndexStat {
            ctime_secs: stat.ctime.secs,
            ctime_nsecs: stat.ctime.nsecs,
            mtime_secs: stat.mtime.secs,
            mtime_nsecs: stat.mtime.nsecs,
            dev: stat.dev,
            ino: stat.ino,
            uid: stat.uid,
            gid: stat.gid,
            size: stat.size,
        }
    }

    pub(crate) fn to_gix(&self) -> gix::index::entry::Stat {
        gix::index::entry::Stat {
            ctime: gix::index::entry::stat::Time {
                secs: self.ctime_secs,
                nsecs: self.ctime_nsecs,
            },
            mtime: gix::index::entry::stat::Time {
                secs: self.mtime_secs,
                nsecs: self.mtime_nsecs,
            },
            dev: self.dev,
            ino: self.ino,
            uid: self.uid,
            gid: self.gid,
            size: self.size,
        }
    }
}

/// Detect whether `stat` is racy if stored in an index with the given timestamp
/// (see racy-git.txt): the entry needs to be re-checked against disk.
#[napi]
pub fn index_stat_is_racy(
    stat: IndexStat,
    timestamp: IndexTimestamp,
    options: Option<IndexStatOptions>,
) -> bool {
    let ft = filetime::FileTime::from_unix_time(timestamp.secs, timestamp.nsecs);
    stat.to_gix()
        .is_racy(ft, options.map(Into::into).unwrap_or_default())
}

/// Compare two stat entries, respecting the comparison knobs (`trustCtime`,
/// `checkStat`, `useNsec`, `useStdev`).
#[napi]
pub fn index_stat_matches(a: IndexStat, b: IndexStat, options: Option<IndexStatOptions>) -> bool {
    a.to_gix()
        .matches(&b.to_gix(), options.map(Into::into).unwrap_or_default())
}

/// A point in time, seconds relative to the unix epoch plus nanoseconds.
#[derive(Clone, Copy)]
#[napi(object)]
pub struct IndexTimestamp {
    pub secs: i64,
    pub nsecs: u32,
}

/// Configuration for comparing stat entries, mirroring `gix_index::entry::Options`.
/// Defaults: `trustCtime`/`checkStat` true, `useNsec`/`useStdev` false.
#[napi(object)]
pub struct IndexStatOptions {
    pub trust_ctime: Option<bool>,
    pub check_stat: Option<bool>,
    pub use_nsec: Option<bool>,
    pub use_stdev: Option<bool>,
}

impl From<IndexStatOptions> for gix::index::entry::stat::Options {
    fn from(o: IndexStatOptions) -> Self {
        gix::index::entry::stat::Options {
            trust_ctime: o.trust_ctime.unwrap_or(true),
            check_stat: o.check_stat.unwrap_or(true),
            use_nsec: o.use_nsec.unwrap_or(false),
            use_stdev: o.use_stdev.unwrap_or(false),
        }
    }
}

fn entry_to_dto(file: &gix::index::File, e: &gix::index::Entry) -> IndexEntry {
    IndexEntry {
        path: lossy(e.path(file)),
        mode: format!("{:?}", e.mode),
        id: e.id.to_string(),
        stage: e.stage() as u32,
        stat: IndexStat::from_gix(e.stat),
        flags: e.flags.bits(),
    }
}

fn stage_from_u32(stage: u32) -> Result<gix::index::entry::Stage> {
    match stage {
        0 => Ok(gix::index::entry::Stage::Unconflicted),
        1 => Ok(gix::index::entry::Stage::Base),
        2 => Ok(gix::index::entry::Stage::Ours),
        3 => Ok(gix::index::entry::Stage::Theirs),
        other => Err(err_str(
            "entry_by_path_and_stage",
            format!(
                "invalid stage {other}; expected 0 (unconflicted), 1 (base), 2 (ours), or 3 (theirs)"
            ),
        )),
    }
}

/// Map a git mode string to a `gix_index::entry::Mode`. Any unrecognized value defaults to a
/// regular file (`100644`), matching git's lenient parsing.
fn string_to_index_mode(mode: &str) -> gix::index::entry::Mode {
    match mode {
        "100755" => gix::index::entry::Mode::FILE_EXECUTABLE,
        "040000" | "40000" => gix::index::entry::Mode::DIR,
        "120000" => gix::index::entry::Mode::SYMLINK,
        "160000" => gix::index::entry::Mode::COMMIT,
        _ => gix::index::entry::Mode::FILE,
    }
}

/// A git index (staging area), wrapping `gix_index::File`.
///
/// Returned by [`Repository::index()`], [`Repository::open_index()`], [`Repository::try_index()`],
/// [`Repository::index_or_load()`] and [`Repository::index_from_tree()`].
#[napi]
pub struct Index {
    pub(crate) inner: gix::index::File,
}

#[napi]
impl Index {
    /// The number of entries in the index.
    #[napi]
    pub fn len(&self) -> u32 {
        self.inner.entries().len() as u32
    }

    /// `true` if the index contains no entries.
    #[napi]
    pub fn is_empty(&self) -> bool {
        self.inner.entries().is_empty()
    }

    /// The path to the index file on disk (`<git_dir>/index`).
    #[napi]
    pub fn path(&self) -> String {
        self.inner.path().to_string_lossy().into_owned()
    }

    /// The on-disk index format version: `2`, `3`, or `4`.
    #[napi]
    pub fn version(&self) -> u32 {
        match self.inner.version() {
            gix::index::Version::V2 => 2,
            gix::index::Version::V3 => 3,
            gix::index::Version::V4 => 4,
        }
    }

    /// All entries currently in the index.
    #[napi]
    pub fn entries(&self) -> Vec<IndexEntry> {
        self.inner
            .entries()
            .iter()
            .map(|e| entry_to_dto(&self.inner, e))
            .collect()
    }

    /// The time at which the index state was created, indicating its freshness
    /// compared to other files on disk.
    #[napi]
    pub fn timestamp(&self) -> IndexTimestamp {
        let t = self.inner.timestamp();
        IndexTimestamp {
            secs: t.unix_seconds(),
            nsecs: t.nanoseconds(),
        }
    }

    /// Update the timestamp of this state, indicating its freshness compared to other
    /// files on disk.
    ///
    /// Be careful: setting a timestamp without correctly updating the index **will cause
    /// (file system) race conditions** — see racy-git.txt in the git documentation.
    #[napi]
    pub fn set_timestamp(&mut self, secs: f64, nsecs: f64) {
        self.inner.set_timestamp(filetime::FileTime::from_unix_time(
            secs as i64,
            nsecs as u32,
        ));
    }

    /// Whether the index is sparse: it contains at least one directory (`DIR`) entry.
    #[napi]
    pub fn is_sparse(&self) -> bool {
        self.inner.is_sparse()
    }

    /// Whether the end-of-index extension was present when this index was decoded.
    #[napi]
    pub fn had_end_of_index_marker(&self) -> bool {
        self.inner.had_end_of_index_marker()
    }

    /// Whether the offset-table extension was present when this index was decoded.
    #[napi]
    pub fn had_offset_table(&self) -> bool {
        self.inner.had_offset_table()
    }

    /// Return the entry at `path` that is either at stage 0, or at stage 2 (ours) in
    /// case of a merge conflict; `null` when absent.
    #[napi]
    pub fn entry_by_path(&self, path: String) -> Option<IndexEntry> {
        self.inner
            .entry_by_path(bstr(&path))
            .map(|e| entry_to_dto(&self.inner, e))
    }

    /// Return the entry at `path` and `stage` (0 = unconflicted, 1 = base, 2 = ours,
    /// 3 = theirs); `null` when absent. Throws on a stage outside 0..=3.
    #[napi]
    pub fn entry_by_path_and_stage(&self, path: String, stage: u32) -> Result<Option<IndexEntry>> {
        let stage = stage_from_u32(stage)?;
        Ok(self
            .inner
            .entry_by_path_and_stage(bstr(&path), stage)
            .map(|e| entry_to_dto(&self.inner, e)))
    }

    /// Return the entry at `path` at any stage. Unlike `entryByPath`, no preference is
    /// given to stage 0 or 2. With `ignoreCase`, the search folds ASCII case.
    ///
    /// Note: implemented as a linear scan rather than gix's hash-accelerated lookup;
    /// results are identical, but large indexes pay O(n) per call.
    #[napi]
    pub fn entry_by_path_icase(&self, path: String, ignore_case: bool) -> Option<IndexEntry> {
        let path = path.as_bytes();
        for e in self.inner.entries() {
            let p = e.path(&self.inner);
            let matched = if ignore_case {
                p.eq_ignore_ascii_case(path)
            } else {
                p == path
            };
            if matched {
                return Some(entry_to_dto(&self.inner, e));
            }
        }
        None
    }

    /// Whether `path` is a directory containing indexed entries or a submodule /
    /// sparse directory itself (case-sensitive).
    #[napi]
    pub fn path_is_directory(&self, path: String) -> bool {
        self.inner.path_is_directory(bstr(&path))
    }

    /// Like `pathIsDirectory`, with optional ASCII case-folding.
    ///
    /// Note: implemented as a linear scan (see `entryByPathIcase`).
    #[napi]
    pub fn path_is_directory_icase(&self, path: String, ignore_case: bool) -> bool {
        let dir = path.as_bytes();
        let is_dir_entry = |e: &gix::index::Entry| e.mode.is_sparse() || e.mode.is_submodule();

        // A directory entry itself (submodule or sparse) matches exactly first.
        for e in self.inner.entries() {
            let p = e.path(&self.inner);
            let matched = if ignore_case {
                p.eq_ignore_ascii_case(dir)
            } else {
                p == dir
            };
            if matched && is_dir_entry(e) {
                return true;
            }
        }
        // Otherwise any entry inside `dir/` counts.
        for e in self.inner.entries() {
            let p = e.path(&self.inner);
            if p.len() > dir.len() && p[dir.len()] == b'/' {
                let prefix = &p[..dir.len()];
                let matched = if ignore_case {
                    prefix.eq_ignore_ascii_case(dir)
                } else {
                    prefix == dir
                };
                if matched {
                    return true;
                }
            }
        }
        false
    }

    /// Unconditionally sort entries (by path, then stage) so lookups work; required
    /// after appending entries with `Repository.editIndex()`-style pushes.
    #[napi]
    pub fn sort_entries(&mut self) {
        self.inner.sort_entries();
    }

    /// Physically remove the entry at `index`, returning it; `null` when out of bounds.
    /// Note that the tree-cache extension (if present) is not invalidated by mutations.
    #[napi]
    pub fn remove_entry_at_index(&mut self, index: u32) -> Option<IndexEntry> {
        let e = self.inner.entries().get(index as usize)?;
        let dto = entry_to_dto(&self.inner, e);
        self.inner.remove_entry_at_index(index as usize);
        Some(dto)
    }

    /// The `tree` (tree-cache) extension, if present.
    #[napi]
    pub fn tree_extension(&self) -> Option<IndexTreeExtension> {
        self.inner.tree().map(tree_extension_to_dto)
    }

    /// The `link` (split index) extension, if present.
    #[napi]
    pub fn link_extension(&self) -> Option<IndexLinkExtension> {
        self.inner.link().map(|link| IndexLinkExtension {
            shared_index_checksum: link.shared_index_checksum.to_string(),
            delete_bits: link.bitmaps.as_ref().map(|b| b.delete.num_bits() as u32),
            replace_bits: link.bitmaps.as_ref().map(|b| b.replace.num_bits() as u32),
        })
    }

    /// Whether the resolve-undo (REUC) extension is present. Its decoded paths are not
    /// publicly accessible in gix-index yet, so only presence is reported.
    #[napi]
    pub fn has_resolve_undo(&self) -> bool {
        self.inner.resolve_undo().is_some()
    }

    /// The untracked-cache (UNTR) extension, if present.
    #[napi]
    pub fn untracked_extension(&self) -> Option<IndexUntrackedCache> {
        self.inner.untracked().map(|cache| IndexUntrackedCache {
            identifier: lossy(cache.identifier()),
            info_exclude: cache.info_exclude().map(oid_stat_to_dto),
            excludes_file: cache.excludes_file().map(oid_stat_to_dto),
            exclude_filename_per_dir: lossy(cache.exclude_filename_per_dir()),
            dir_flags: cache.dir_flags(),
            directories: cache
                .directories()
                .iter()
                .map(|d| IndexUntrackedDirectory {
                    name: lossy(d.name()),
                    untracked_entries: d.untracked_entries().iter().map(|n| lossy(n)).collect(),
                    sub_directory_indices: d.sub_directories().iter().map(|i| *i as u32).collect(),
                    stat: d.stat().map(|s| IndexStat::from_gix(*s)),
                    exclude_file_oid: d.exclude_file_oid().map(|o| o.to_string()),
                    check_only: d.check_only(),
                })
                .collect(),
        })
    }

    /// Whether the fs-monitor (FSMN) extension is present. Its token and dirty-bitmap
    /// are not publicly accessible in gix-index yet, so only presence is reported.
    #[napi]
    pub fn has_fs_monitor(&self) -> bool {
        self.inner.fs_monitor().is_some()
    }

    /// Write the index to its path after acquiring a lock file. Returns the version
    /// that was actually written. With `skipHash`, the trailing checksum is zeroed
    /// (roughly 2x faster, at the cost of corruption detection — `index.skipHash`).
    #[napi]
    pub fn write(&mut self, skip_hash: Option<bool>) -> Result<u32> {
        let options = gix::index::write::Options {
            skip_hash: skip_hash.unwrap_or(false),
            ..Default::default()
        };
        self.inner
            .write(options)
            .map_err(|e| err("write_index", e))?;
        // `File::write` returns () but updates the in-memory state's version to what
        // was written, so read it back from there.
        Ok(match self.inner.version() {
            gix::index::Version::V2 => 2,
            gix::index::Version::V3 => 3,
            gix::index::Version::V4 => 4,
        })
    }

    /// Serialize the index to bytes without touching the file system (useful for bare
    /// repositories, archives, and network transport). Returns the written version, the
    /// trailing checksum (absent when `skipHash`), and the encoded bytes.
    #[napi]
    pub fn write_to_buffer(&self, skip_hash: Option<bool>) -> Result<IndexWriteOutcome> {
        let options = gix::index::write::Options {
            skip_hash: skip_hash.unwrap_or(false),
            ..Default::default()
        };
        let mut out = Vec::new();
        let (version, hash) = self
            .inner
            .write_to(&mut out, options)
            .map_err(|e| err("write_index_to_buffer", e))?;
        Ok(IndexWriteOutcome {
            version: match version {
                gix::index::Version::V2 => 2,
                gix::index::Version::V3 => 3,
                gix::index::Version::V4 => 4,
            },
            checksum: (!hash.is_null()).then(|| hash.to_string()),
            data: Buffer::from(out),
        })
    }
}

/// The `tree` (tree-cache) extension: cached directory trees to speed up tree building.
#[napi(object)]
pub struct IndexTreeExtension {
    /// The name of the tree/directory, empty for the root tree.
    pub name: String,
    /// The id of the directory tree of the associated tree object.
    pub id: String,
    /// The amount of non-tree items in this directory tree, recursively; `null` when invalid.
    pub num_entries: Option<u32>,
    /// The child trees below this tree.
    pub children: Vec<IndexTreeExtension>,
}

fn tree_extension_to_dto(tree: &gix::index::extension::Tree) -> IndexTreeExtension {
    IndexTreeExtension {
        name: lossy(&tree.name),
        id: tree.id.to_string(),
        num_entries: tree.num_entries,
        children: tree.children.iter().map(tree_extension_to_dto).collect(),
    }
}

/// The `link` (split index) extension.
#[napi(object)]
pub struct IndexLinkExtension {
    /// The checksum of the shared index as last seen.
    pub shared_index_checksum: String,
    /// Number of bits in the delete bitmap, if bitmaps are present.
    pub delete_bits: Option<u32>,
    /// Number of bits in the replace bitmap, if bitmaps are present.
    pub replace_bits: Option<u32>,
}

/// The untracked-cache (UNTR) extension.
#[napi(object)]
pub struct IndexUntrackedCache {
    /// Something identifying the location and machine this cache is for.
    pub identifier: String,
    /// Stat and object id for the `.git/info/exclude` file, if available.
    pub info_exclude: Option<IndexOidStat>,
    /// Stat and object id for the `core.excludesFile`, if available.
    pub excludes_file: Option<IndexOidStat>,
    /// Usually `.gitignore`.
    pub exclude_filename_per_dir: String,
    /// The directory flags git used while populating the cache.
    pub dir_flags: u32,
    /// Directories and sub-directories; `directories[0]` is the root.
    pub directories: Vec<IndexUntrackedDirectory>,
}

/// Stat and object id linking a worktree file with what's in the ODB.
#[napi(object)]
pub struct IndexOidStat {
    pub stat: IndexStat,
    pub id: String,
}

fn oid_stat_to_dto(o: &gix::index::extension::untracked_cache::OidStat) -> IndexOidStat {
    IndexOidStat {
        stat: IndexStat::from_gix(*o.stat()),
        id: o.id().to_string(),
    }
}

/// A directory with information about its untracked files and sub-directories.
#[napi(object)]
pub struct IndexUntrackedDirectory {
    /// The directory name, empty for the root directory.
    pub name: String,
    /// Untracked file and directory names.
    pub untracked_entries: Vec<String>,
    /// Indices into the cache's directory list for sub-directories.
    pub sub_directory_indices: Vec<u32>,
    /// The directory's stat data, if available and valid.
    pub stat: Option<IndexStat>,
    /// The oid of a `.gitignore` file, if it exists.
    pub exclude_file_oid: Option<String>,
    /// Whether git marked this directory as check-only.
    pub check_only: bool,
}

/// The outcome of serializing an index to bytes.
#[napi(object)]
pub struct IndexWriteOutcome {
    /// The index version that was written (`2`, `3`, or `4`).
    pub version: u32,
    /// The trailing checksum, or `null` when skipped.
    pub checksum: Option<String>,
    /// The encoded index bytes, including the trailing checksum.
    pub data: Buffer,
}

/// Options for opening an index file.
#[napi(object)]
pub struct IndexOpenOptions {
    /// Skip the trailing-checksum verification when opening (roughly 2x faster, at the
    /// cost of corruption detection).
    pub skip_hash: Option<bool>,
    /// If set, limit parallel decoding to this many threads.
    pub thread_limit: Option<u32>,
    /// Verify the file against this expected checksum (relevant for shared indexes).
    pub expected_checksum: Option<String>,
    /// Maximum size in bytes of a single allocation caused by untrusted index data.
    pub alloc_limit_bytes: Option<f64>,
}

#[napi]
impl Repository {
    /// Open the shared, auto-updating worktree index; fails if no index exists yet.
    #[napi]
    pub fn index(&self) -> Result<Index> {
        let r = thread_local(&self.inner);
        let shared = r.index().map_err(|e| err("index", e))?;
        Ok(Index {
            inner: gix::index::File::clone(&shared),
        })
    }

    /// Open a fresh, fully-decoded copy of the index file; fails if no index exists yet.
    /// With `skipHash`, the on-disk checksum verification is skipped (faster, less safe).
    #[napi]
    pub fn open_index(&self, skip_hash: Option<bool>) -> Result<Index> {
        let r = thread_local(&self.inner);
        let file = gix::index::File::at(
            r.index_path(),
            r.object_hash(),
            skip_hash.unwrap_or(false),
            Default::default(),
        )
        .map_err(|e| err("open_index", e))?;
        Ok(Index { inner: file })
    }

    /// Open an index file at an arbitrary `path` (not necessarily this repository's
    /// index), creating an empty in-memory index at that path if the file does not
    /// exist yet. A missing *shared* index remains an error.
    #[napi]
    pub fn open_index_at(&self, path: String, options: Option<IndexOpenOptions>) -> Result<Index> {
        let r = thread_local(&self.inner);
        let options = options.unwrap_or(IndexOpenOptions {
            skip_hash: None,
            thread_limit: None,
            expected_checksum: None,
            alloc_limit_bytes: None,
        });
        let expected_checksum = options
            .expected_checksum
            .as_deref()
            .map(parse_oid)
            .transpose()?;
        let decode_options = gix::index::decode::Options {
            thread_limit: options.thread_limit.map(|t| t as usize),
            min_extension_block_in_bytes_for_threading: 0,
            expected_checksum,
            alloc_limit_bytes: options.alloc_limit_bytes.map(|b| b as usize),
        };
        let file = gix::index::File::at_or_default(
            path,
            r.object_hash(),
            options.skip_hash.unwrap_or(false),
            decode_options,
        )
        .map_err(|e| err("open_index_at", e))?;
        Ok(Index { inner: file })
    }

    /// Open a fresh, fully-decoded copy of the index file; fails if no index exists yet.
    #[napi]
    pub fn open_index_file(&self) -> Result<Index> {
        let r = thread_local(&self.inner);
        let file = r.open_index().map_err(|e| err("open_index", e))?;
        Ok(Index { inner: file })
    }

    /// Return the index if present, or `null` if the repository has no index file yet.
    #[napi]
    pub fn try_index(&self) -> Result<Option<Index>> {
        let r = thread_local(&self.inner);
        let opt = r.try_index().map_err(|e| err("try_index", e))?;
        Ok(opt.map(|shared| Index {
            inner: gix::index::File::clone(&shared),
        }))
    }

    #[napi]
    pub fn index_or_empty(&self) -> Result<Index> {
        let r = thread_local(&self.inner);
        let shared = r.index_or_empty().map_err(|e| err("index_or_empty", e))?;
        Ok(Index {
            inner: gix::index::File::clone(&shared),
        })
    }

    /// Return the index loaded from `HEAD^{tree}` if no index file exists, or an empty
    /// index if `HEAD` is unborn. Never fails due to a missing index.
    #[napi]
    pub fn index_or_load(&self) -> Result<Index> {
        let r = thread_local(&self.inner);
        let loaded = r
            .index_or_load_from_head_or_empty()
            .map_err(|e| err("index_or_load", e))?;
        Ok(Index {
            inner: gix::index::File::clone(&loaded),
        })
    }

    /// Load the index file, or expand the HEAD tree into an in-memory index when no
    /// index file exists. Unlike `indexOrLoad`, this fails on an unborn HEAD instead of
    /// falling back to an empty index.
    #[napi]
    pub fn index_or_load_from_head(&self) -> Result<Index> {
        let r = thread_local(&self.inner);
        let loaded = r
            .index_or_load_from_head()
            .map_err(|e| err("index_or_load_from_head", e))?;
        Ok(Index {
            inner: gix::index::File::clone(&loaded),
        })
    }

    /// Build an in-memory index from a tree id (expensive: recursively unpacks the tree).
    #[napi]
    pub fn index_from_tree(&self, tree: String) -> Result<Index> {
        let r = thread_local(&self.inner);
        let oid = parse_oid(&tree)?;
        let file = r
            .index_from_tree(&oid)
            .map_err(|e| err("index_from_tree", e))?;
        Ok(Index { inner: file })
    }

    /// The path at which the index file would live (`<git_dir>/index`).
    #[napi]
    pub fn index_path(&self) -> String {
        let r = thread_local(&self.inner);
        r.index_path().to_string_lossy().into_owned()
    }

    /// Read entries from the index (fails if no index exists).
    #[napi]
    pub fn index_entries(&self) -> Result<Vec<IndexEntry>> {
        Ok(self.open_index_file()?.entries())
    }

    /// Read entries from the index, or load them from `HEAD` / an empty index if absent.
    #[napi]
    pub fn index_or_load_from_head_entries(&self) -> Result<Vec<IndexEntry>> {
        Ok(self.index_or_load()?.entries())
    }

    /// Add or update index entries in the repository index file.
    #[napi]
    pub fn edit_index(&self, entries: Vec<IndexEntryInput>) -> Result<()> {
        let r = thread_local(&self.inner);
        let index_path = r.index_path();
        let mut file =
            match gix::index::File::at(&index_path, r.object_hash(), false, Default::default()) {
                Ok(file) => file,
                Err(_) => gix::index::File::from_state(
                    gix::index::State::new(r.object_hash()),
                    index_path,
                ),
            };

        for e in entries {
            let mode = string_to_index_mode(e.mode.as_deref().unwrap_or("100644"));
            let oid = parse_oid(&e.id)?;
            let path = gix::bstr::BString::from(e.path);
            file.dangerously_push_entry(
                gix::index::entry::Stat::default(),
                oid,
                gix::index::entry::Flags::empty(),
                mode,
                path.as_ref(),
            );
        }

        // `dangerously_push_entry` breaks the sorted invariant; restore it before writing,
        // or the index would be rejected as corrupt by git. The tree-cache extension is
        // written as-is and would be stale after this change, so drop it (the documented
        // workaround until gix recomputes it on write).
        file.sort_entries();
        file.remove_tree();
        file.write(Default::default())
            .map_err(|e| err("write_index", e))?;
        Ok(())
    }
}

/// A git index entry mode, wrapping `gix_index::entry::Mode`.
///
/// Bitflags-backed; distinguishes a regular file, executable file, symbolic link, submodule
/// (commit), and sparse/cache directory (`DIR`). Convert to/from a tree entry mode with
/// `to_tree_entry_mode()` and `from_tree_mode()`.
///
/// Note: `IndexEntry.mode` (in `crate::repository`) is still exposed as a string for backward
/// compatibility; construct this type to access the structured predicates.
#[napi]
pub struct IndexEntryMode {
    pub(crate) inner: gix::index::entry::Mode,
}

#[napi]
impl IndexEntryMode {
    /// Parse an `IndexEntryMode` from a git mode string: `100644`, `100755`,
    /// `040000`/`40000`, `120000`, `160000`. Any other value defaults to `100644`
    /// (a regular file), matching git's lenient parsing.
    #[napi(constructor)]
    pub fn new(mode: String) -> Self {
        IndexEntryMode {
            inner: string_to_index_mode(&mode),
        }
    }

    /// The raw bitflags value (e.g. `0o100644` for a regular file).
    #[napi(getter)]
    pub fn bits(&self) -> u32 {
        self.inner.bits()
    }

    /// `true` if this is a sparse directory entry (points at a directory not normally tracked).
    #[napi]
    pub fn is_sparse(&self) -> bool {
        self.inner.is_sparse()
    }

    /// `true` if this is a submodule entry.
    #[napi]
    pub fn is_submodule(&self) -> bool {
        self.inner.is_submodule()
    }

    /// Convert this index mode to the corresponding tree entry mode, as a canonical git mode
    /// string (`100644`, `100755`, `040000`, `120000`, `160000`). Returns `null` when the bits
    /// do not map to a known tree mode.
    #[napi]
    pub fn to_tree_entry_mode(&self) -> Option<String> {
        self.inner.to_tree_entry_mode().map(|m| {
            let mut backing = [0u8; 6];
            m.as_bytes(&mut backing).to_string()
        })
    }

    /// Build an `IndexEntryMode` from a tree entry mode string (`100644`, `100755`, `040000`,
    /// `120000`, `160000`). Throws on an unrecognized value.
    #[napi]
    pub fn from_tree_mode(mode: String) -> Result<Self> {
        let val = u32::from_str_radix(mode.trim(), 8).map_err(|e| {
            err_str(
                "from_tree_mode",
                format!("invalid octal mode '{mode}': {e}"),
            )
        })?;
        let tree_mode = gix::object::tree::EntryMode::try_from(val)
            .map_err(|_| err_str("from_tree_mode", format!("unsupported tree mode '{mode}'")))?;
        Ok(IndexEntryMode {
            inner: tree_mode.into(),
        })
    }

    /// Build an `IndexEntryMode` from its raw bitflags value (e.g. `0o100644`).
    #[napi]
    pub fn from_bits(bits: u32) -> Option<IndexEntryMode> {
        gix::index::entry::Mode::from_bits(bits).map(|m| IndexEntryMode { inner: m })
    }
}

/// Filesystem metadata for a path, mirroring `gix_index::fs::Metadata`.
///
/// Obtained via [`lstat()`], which does not follow symlinks; this is the same `lstat`-based
/// metadata git uses when populating index entries. On non-unix platforms the device/inode/user
/// ids and the executable/symlink bits fall back to platform defaults (0 / false).
#[napi]
pub struct IndexFsMetadata {
    inner: gix::index::fs::Metadata,
}

fn system_time_to_index_timestamp(t: std::time::SystemTime) -> IndexTimestamp {
    let ft = filetime::FileTime::from_system_time(t);
    IndexTimestamp {
        secs: ft.unix_seconds(),
        nsecs: ft.nanoseconds(),
    }
}

#[napi]
impl IndexFsMetadata {
    /// `true` if the path is a directory.
    #[napi(getter)]
    pub fn is_dir(&self) -> bool {
        self.inner.is_dir()
    }

    /// `true` if the path is a regular file (executable or not).
    #[napi(getter)]
    pub fn is_file(&self) -> bool {
        self.inner.is_file()
    }

    /// `true` if the path is a symbolic link.
    #[napi(getter)]
    pub fn is_symlink(&self) -> bool {
        self.inner.is_symlink()
    }

    /// `true` if the file's executable bit is set (false on non-unix platforms).
    #[napi(getter)]
    pub fn is_executable(&self) -> bool {
        self.inner.is_executable()
    }

    /// The size of the file in bytes.
    #[napi(getter)]
    pub fn len(&self) -> i64 {
        self.inner.len() as i64
    }

    /// The device id on which the file lives (0 on non-unix platforms).
    #[napi(getter)]
    pub fn dev(&self) -> i64 {
        self.inner.dev() as i64
    }

    /// The inode id tracking the file (0 on non-unix platforms).
    #[napi(getter)]
    pub fn ino(&self) -> i64 {
        self.inner.ino() as i64
    }

    /// The user id of the file's owner (0 on non-unix platforms).
    #[napi(getter)]
    pub fn uid(&self) -> u32 {
        self.inner.uid()
    }

    /// The group id of the file's owning group (0 on non-unix platforms).
    #[napi(getter)]
    pub fn gid(&self) -> u32 {
        self.inner.gid()
    }

    /// The time the file was last modified, or `null` if unavailable.
    #[napi(getter)]
    pub fn modified(&self) -> Option<IndexTimestamp> {
        self.inner.modified().map(system_time_to_index_timestamp)
    }

    /// The inode change time (ctime), or `null` if unavailable.
    #[napi(getter)]
    pub fn created(&self) -> Option<IndexTimestamp> {
        self.inner.created().map(system_time_to_index_timestamp)
    }
}

/// Stat `path` without following symlinks, returning its filesystem metadata
/// (the same `lstat`-based metadata git uses for index entries).
#[napi]
pub fn lstat(path: String) -> Result<IndexFsMetadata> {
    let md = gix::index::fs::Metadata::from_path_no_follow(std::path::Path::new(&path))
        .map_err(|e| err("lstat", e))?;
    Ok(IndexFsMetadata { inner: md })
}

/// A 4-character ASCII identifier for a chunk, plus its byte range in the file.
#[napi(object)]
pub struct ChunkEntry {
    /// The 4-character chunk kind, e.g. `"OIDF"` or `"DATA"`.
    pub kind: String,
    /// Start offset of the chunk, relative to the beginning of the file.
    pub start: i64,
    /// End offset of the chunk (exclusive).
    pub end: i64,
}

/// A byte range `[start, end)` for a chunk.
#[napi(object)]
pub struct ChunkOffset {
    /// Start offset of the chunk.
    pub start: i64,
    /// End offset of the chunk (exclusive).
    pub end: i64,
}

/// One chunk's data to write when serializing a chunk file.
#[napi(object)]
pub struct ChunkInput {
    /// The 4-character chunk kind, e.g. `"OIDF"`.
    pub kind: String,
    /// The exact bytes of the chunk; its length must match the size planned via `planChunk`.
    pub data: Buffer,
}

/// A chunk-file table of contents, mirroring `gix_chunk::file::Index`.
///
/// Use [`ChunkIndex::from_bytes`] to read an existing chunk file's index (commit-graph,
/// multi-pack-index, etc.), or [`ChunkIndex::for_writing`] + [`ChunkIndex::plan_chunk`] +
/// [`ChunkIndex::write`] to produce one.
#[napi]
pub struct ChunkIndex {
    inner: gix_chunk::file::Index,
    // Only populated for indexes decoded from bytes (used by `chunks()`); empty otherwise.
    data: Vec<u8>,
    toc_offset: usize,
    num_chunks: u32,
}

/// Parse a 4-byte chunk id, or throw if it isn't exactly 4 bytes.
fn chunk_id(kind: &str) -> Result<[u8; 4]> {
    let b = kind.as_bytes();
    if b.len() != 4 {
        return Err(err_str(
            "chunk_id",
            format!(
                "chunk id must be exactly 4 bytes, got '{}' ({} bytes)",
                kind,
                b.len()
            ),
        ));
    }
    let mut id = [0u8; 4];
    id.copy_from_slice(b);
    Ok(id)
}

#[napi]
impl ChunkIndex {
    /// Decode a chunk-file table of contents from `data`, starting at `tocOffset`, containing
    /// `numChunks` chunks. Throws on a malformed table of contents.
    #[napi]
    pub fn from_bytes(data: Buffer, toc_offset: u32, num_chunks: u32) -> Result<ChunkIndex> {
        let idx = gix_chunk::file::Index::from_bytes(&data, toc_offset as usize, num_chunks)
            .map_err(|e| err("chunk_index_from_bytes", e))?;
        Ok(ChunkIndex {
            inner: idx,
            data: data.to_vec(),
            toc_offset: toc_offset as usize,
            num_chunks,
        })
    }

    /// Create an empty chunk index for planning and writing chunks.
    #[napi]
    pub fn for_writing() -> ChunkIndex {
        ChunkIndex {
            inner: gix_chunk::file::Index::for_writing(),
            data: Vec::new(),
            toc_offset: 0,
            num_chunks: 0,
        }
    }

    /// The number of chunks in this index.
    #[napi]
    pub fn num_chunks(&self) -> u32 {
        self.inner.num_chunks() as u32
    }

    /// The end offset of the last chunk — the highest offset in the file. Only meaningful for
    /// indexes decoded from bytes.
    #[napi]
    pub fn highest_offset(&self) -> i64 {
        self.inner.highest_offset() as i64
    }

    /// Return the byte range `[start, end)` of the chunk identified by `kind` (a 4-character id).
    #[napi]
    pub fn offset_by_id(&self, kind: String) -> Result<ChunkOffset> {
        let id = chunk_id(&kind)?;
        let r = self
            .inner
            .usize_offset_by_id(id)
            .map_err(|e| err("chunk_index_offset", e))?;
        Ok(ChunkOffset {
            start: r.start as i64,
            end: r.end as i64,
        })
    }

    /// Return the raw bytes of the chunk identified by `kind` (a 4-character id) from `data`.
    #[napi]
    pub fn data_by_id(&self, data: Buffer, kind: String) -> Result<Buffer> {
        let id = chunk_id(&kind)?;
        let slice = self
            .inner
            .data_by_id(&data, id)
            .map_err(|e| err("chunk_index_data", e))?;
        Ok(Buffer::from(slice.to_vec()))
    }

    /// Enumerate all chunks as `{ kind, start, end }` by decoding the table of contents in `data`.
    #[napi]
    pub fn chunks(&self, data: Buffer) -> Result<Vec<ChunkEntry>> {
        let entry_size = gix_chunk::file::Index::ENTRY_SIZE;
        let n = self.num_chunks as usize;
        let min_len = self.toc_offset + (n + 1) * entry_size;
        if data.len() < min_len {
            return Err(err_str(
                "chunk_index_chunks",
                format!(
                    "data too small for table of contents: need {min_len} bytes, got {}",
                    data.len()
                ),
            ));
        }
        let mut offsets = Vec::with_capacity(n + 1);
        for i in 0..=n {
            let at = self.toc_offset + i * entry_size + 4; // skip the 4-byte kind
            let o = u64::from_be_bytes(data[at..at + 8].try_into().unwrap());
            offsets.push(o as i64);
        }
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let at = self.toc_offset + i * entry_size;
            let kind = String::from_utf8_lossy(&data[at..at + 4]).into_owned();
            out.push(ChunkEntry {
                kind,
                start: offsets[i],
                end: offsets[i + 1],
            });
        }
        Ok(out)
    }

    /// Plan a chunk to write; `exactSizeOnDisk` is the exact number of bytes the chunk will occupy.
    /// Must be called on an index created via [`ChunkIndex::for_writing`].
    #[napi]
    pub fn plan_chunk(&mut self, kind: String, exact_size_on_disk: i64) -> Result<()> {
        let id = chunk_id(&kind)?;
        self.inner.plan_chunk(id, exact_size_on_disk as u64);
        Ok(())
    }

    /// The total size, in bytes, of all planned chunks thus far. Must be called on an index
    /// created via [`ChunkIndex::for_writing`].
    #[napi]
    pub fn planned_storage_size(&self) -> i64 {
        self.inner.planned_storage_size() as i64
    }

    /// Write the planned chunks, in order, using the provided chunk data, returning the serialized
    /// chunk file (table of contents + chunks + sentinel). Each `ChunkInput.data` length must match
    /// the size planned for that `kind` via [`ChunkIndex::plan_chunk`].
    #[napi]
    pub fn write(&mut self, chunks: Vec<ChunkInput>) -> Result<Buffer> {
        // Steal the owned `Index` from `self` so we can hand it to `into_write`, leaving a
        // fresh (writing) index behind in its place.
        let idx = std::mem::replace(&mut self.inner, gix_chunk::file::Index::for_writing());
        let mut out: Vec<u8> = Vec::new();
        let mut writer = idx
            .into_write(&mut out, 0)
            .map_err(|e| err("chunk_index_write", e))?;

        let mut by_kind: std::collections::HashMap<[u8; 4], Buffer> =
            std::collections::HashMap::new();
        for c in chunks {
            let id = chunk_id(&c.kind)?;
            if by_kind.insert(id, c.data).is_some() {
                return Err(err_str(
                    "chunk_index_write",
                    format!("duplicate chunk data for '{}'", c.kind),
                ));
            }
        }

        while let Some(kind) = writer.next_chunk() {
            let data = by_kind.remove(&kind).ok_or_else(|| {
                err_str(
                    "chunk_index_write",
                    format!("missing chunk data for {kind:?}"),
                )
            })?;
            writer
                .write_all(&data)
                .map_err(|e| err("chunk_index_write", e))?;
        }

        let _ = writer.into_inner();
        Ok(Buffer::from(out))
    }
}
