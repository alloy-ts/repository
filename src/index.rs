use crate::error::RepositoryResult as Result;
use gix::bstr::ByteSlice;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

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

pub(crate) fn entry_to_dto(file: &gix::index::File, e: &gix::index::Entry) -> IndexEntry {
    IndexEntry {
        path: lossy(e.path(file)),
        mode: format!("{:?}", e.mode),
        id: e.id.to_string(),
        stage: e.stage() as u32,
        stat: IndexStat::from_gix(e.stat),
        flags: e.flags.bits(),
    }
}

fn stage_from_u32(stage: u32) -> napi::Result<gix::index::entry::Stage> {
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

/// A merge conflict: the three sides of an entry sharing a path, any of which may be
/// absent (for example a delete/modify conflict has no ancestor side).
#[napi(object)]
pub struct IndexConflict {
    /// Stage 1 — the common ancestor side, or `null`.
    pub ancestor: Option<IndexEntry>,
    /// Stage 2 — our side, or `null`.
    pub our: Option<IndexEntry>,
    /// Stage 3 — their side, or `null`.
    pub theirs: Option<IndexEntry>,
}

/// Push a single entry into an in-memory index, restoring the sorted invariant and
/// dropping the (now-stale) tree-cache extension — mirrors `Repository.editIndex`.
pub(crate) fn push_index_entry(
    file: &mut gix::index::File,
    entry: &IndexEntryInput,
) -> napi::Result<()> {
    let mode = string_to_index_mode(entry.mode.as_deref().unwrap_or("100644"));
    let oid = parse_oid(&entry.id)?;
    let path = gix::bstr::BString::from(entry.path.as_bytes());
    file.dangerously_push_entry(
        gix::index::entry::Stat::default(),
        oid,
        gix::index::entry::Flags::empty(),
        mode,
        path.as_ref(),
    );
    file.sort_entries();
    file.remove_tree();
    Ok(())
}

/// Recursively stage every file in `root`'s worktree into `index`, mirroring
/// `git add -A`. Ignored files are staged too (matching git_spawn's
/// `Commit::commit`, which calls `index.add_all(["*"], …)` with no ignore
/// callback). `present` collects the git-style (forward-slash) paths that were
/// staged so the caller can drop index entries no longer on disk.
fn stage_dir(
    root: &Path,
    dir: &Path,
    repo: &gix::Repository,
    index: &mut gix::index::File,
    present: &mut Vec<Vec<u8>>,
) -> std::io::Result<()> {
    // Clone the object store once; `into_arc` yields the store type gix exposes
    // for writing blobs (the same call `add_from_buffer_to_index` uses).
    let odb = repo
        .objects
        .clone()
        .into_arc()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_name().as_os_str() == OsStr::new(".git") {
            continue;
        }
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            stage_dir(root, &path, repo, index, present)?;
        } else if meta.is_symlink() {
            let target = std::fs::read_link(&path)?;
            let data = target.to_string_lossy().into_owned().into_bytes();
            let oid = gix::objs::Write::write_buf(&*odb, gix::object::Kind::Blob, &data)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            let rel = relative_posix(root, &path);
            push_index_entry(
                index,
                &IndexEntryInput {
                    path: rel.clone(),
                    id: oid.to_string(),
                    mode: Some("120000".to_string()),
                },
            );
            present.push(rel.into_bytes());
        } else if meta.is_file() {
            let data = std::fs::read(&path)?;
            let oid = gix::objs::Write::write_buf(&*odb, gix::object::Kind::Blob, &data)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            let mode = if meta.permissions().mode() & 0o111 != 0 {
                "100755"
            } else {
                "100644"
            };
            let rel = relative_posix(root, &path);
            push_index_entry(
                index,
                &IndexEntryInput {
                    path: rel.clone(),
                    id: oid.to_string(),
                    mode: Some(mode.to_string()),
                },
            );
            present.push(rel.into_bytes());
        }
        // Other file types (sockets, FIFOs, …) are skipped.
    }
    Ok(())
}

/// Relative path of `path` under `root`, using forward slashes (git path style).
fn relative_posix(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Build a git tree object from a flat list of index entries `(path, mode, oid)` and
/// write it (and any subtrees) to `odb`, returning the root tree id. Only stage-0
/// entries feed the tree; callers must reject sparse indexes beforehand.
fn write_tree_from_index_entries(
    odb: &gix::OdbHandleArc,
    entries: &[(Vec<u8>, gix::index::entry::Mode, gix::ObjectId)],
) -> napi::Result<gix::ObjectId> {
    let mut items: Vec<(Vec<&[u8]>, gix::index::entry::Mode, gix::ObjectId)> = entries
        .iter()
        .map(|(p, m, id)| (p.split(|&b| b == b'/').collect(), *m, *id))
        .collect();
    build_subtree(odb, &mut items)
}

/// Recursively assemble and write one tree level. `items` are entries whose first path
/// component belongs at this level; the remainder of each path lives one level down.
fn build_subtree(
    odb: &gix::OdbHandleArc,
    items: &mut [(Vec<&[u8]>, gix::index::entry::Mode, gix::ObjectId)],
) -> napi::Result<gix::ObjectId> {
    let mut leaves: Vec<(Vec<u8>, gix::index::entry::Mode, gix::ObjectId)> = Vec::new();
    let mut groups: BTreeMap<Vec<u8>, Vec<(Vec<&[u8]>, gix::index::entry::Mode, gix::ObjectId)>> =
        BTreeMap::new();
    for (mut comps, mode, oid) in items.iter().cloned() {
        let name = comps.remove(0);
        if comps.is_empty() {
            leaves.push((name.to_vec(), mode, oid));
        } else {
            groups
                .entry(name.to_vec())
                .or_default()
                .push((comps, mode, oid));
        }
    }

    let mut tree_entries: Vec<gix::objs::tree::Entry> = Vec::new();
    for (name, mut children) in groups {
        let subtree_oid = build_subtree(odb, &mut children)?;
        tree_entries.push(gix::objs::tree::Entry {
            mode: gix::objs::tree::EntryKind::Tree.into(),
            filename: gix::bstr::BString::from(name),
            oid: subtree_oid,
        });
    }
    for (name, mode, oid) in leaves {
        let tree_mode = match mode.to_tree_entry_mode() {
            Some(m) => m,
            None => continue,
        };
        tree_entries.push(gix::objs::tree::Entry {
            mode: tree_mode,
            filename: gix::bstr::BString::from(name),
            oid,
        });
    }

    // Git orders tree entries by name, but treats directory (tree) entries as if their
    // name ended with `/`, so `a` sorts before `a/`, and `a/` before `a.txt`.
    tree_entries.sort_by(|a, b| tree_sort_key(&a).cmp(&tree_sort_key(&b)));
    let tree = gix::objs::Tree {
        entries: tree_entries,
    };
    let mut buf = Vec::new();
    gix::objs::WriteTo::write_to(&tree, &mut buf).map_err(|e| err("write_tree", e))?;
    let oid = gix::objs::Write::write_buf(&*odb, gix::object::Kind::Tree, &buf)
        .map_err(|e| err("write_tree", e))?;
    Ok(oid)
}

/// Key used to sort tree entries the way git does (see `build_subtree`).
fn tree_sort_key(e: &gix::objs::tree::Entry) -> Vec<u8> {
    let mut k = e.filename.to_vec();
    if e.mode == gix::objs::tree::EntryKind::Tree.into() {
        k.push(b'/');
    }
    k
}

/// Simplified fnmatch matching for `removeAll`: a path matches a spec when it equals
/// the spec, lives under a directory spec (`spec/`), or matches a `*` glob.
fn pathspec_matches(path: &[u8], specs: &[String]) -> bool {
    specs.iter().any(|s| spec_matches(path, s.as_bytes()))
}

fn spec_matches(path: &[u8], spec: &[u8]) -> bool {
    if spec.is_empty() {
        return true;
    }
    if spec == path {
        return true;
    }
    if spec.last() == Some(&b'/') {
        return path.starts_with(spec);
    }
    if path.len() > spec.len() && &path[..spec.len()] == spec && path[spec.len()] == b'/' {
        return true;
    }
    if spec.contains(&b'*') {
        return glob_match(spec, path);
    }
    false
}

/// Minimal glob: split the pattern on `*`, requiring each literal segment to appear in
/// order in `text` (a trailing `*` is implicit).
fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    let mut rest = text;
    for (i, seg) in pattern.split(|&b| b == b'*').enumerate() {
        if seg.is_empty() {
            continue;
        }
        if i == 0 {
            if !rest.starts_with(seg) {
                return false;
            }
            rest = &rest[seg.len()..];
        } else if let Some(pos) = rest.windows(seg.len()).position(|w| w == seg) {
            rest = &rest[pos + seg.len()..];
        } else {
            return false;
        }
    }
    true
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
    pub fn entry_by_path_and_stage(
        &self,
        path: String,
        stage: u32,
    ) -> napi::Result<Option<IndexEntry>> {
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
    pub fn write(&mut self, skip_hash: Option<bool>) -> napi::Result<u32> {
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
    pub fn write_to_buffer(&self, skip_hash: Option<bool>) -> napi::Result<IndexWriteOutcome> {
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

    /// Return the entry at position `index` in the (sorted) entry list, or `null` if out
    /// of bounds. Unlike `entries()`, this reads a single entry without cloning the
    /// whole list — useful for very large indexes.
    #[napi]
    pub fn entry_at_index(&self, index: u32) -> Option<IndexEntry> {
        self.inner
            .entries()
            .get(index as usize)
            .map(|e| entry_to_dto(&self.inner, e))
    }

    /// Remove *all* entries, producing an empty in-memory index. The on-disk file is not
    /// touched until `write()` is called.
    #[napi]
    pub fn clear(&mut self) {
        let hash = self.inner.object_hash();
        let path = self.inner.path().to_path_buf();
        self.inner = gix::index::File::from_state(gix::index::State::new(hash), path);
    }

    /// Re-read the index from its file on disk, discarding any in-memory changes. With
    /// `skipHash`, the on-disk checksum verification is skipped (faster, less safe).
    #[napi]
    pub fn read(&mut self, skip_hash: Option<bool>) -> napi::Result<()> {
        let path = self.inner.path().to_path_buf();
        let hash = self.inner.object_hash();
        self.inner =
            gix::index::File::at(path, hash, skip_hash.unwrap_or(false), Default::default())
                .map_err(|e| err("read_index", e))?;
        Ok(())
    }

    /// Remove the entry at `path` and `stage` (default stage 0 / unconflicted). A no-op
    /// when no such entry exists.
    #[napi]
    pub fn remove(&mut self, path: String, stage: Option<u32>) -> napi::Result<()> {
        let stage = stage_from_u32(stage.unwrap_or(0))?;
        if let Some(i) = self.inner.entry_index_by_path_and_stage(bstr(&path), stage) {
            self.inner.remove_entry_at_index(i);
        }
        Ok(())
    }

    /// Remove the entry (all stages) at `path`. A no-op when no such path exists.
    #[napi]
    pub fn remove_path(&mut self, path: String) -> napi::Result<()> {
        for s in [0u32, 1, 2, 3] {
            let stage = stage_from_u32(s)?;
            while let Some(i) = self.inner.entry_index_by_path_and_stage(bstr(&path), stage) {
                self.inner.remove_entry_at_index(i);
            }
        }
        Ok(())
    }

    /// Remove every entry whose path is `path` or lives directly underneath `path/`
    /// (a directory removal). With `stage` set, only entries at that stage are removed;
    /// otherwise every stage is removed.
    #[napi]
    pub fn remove_dir(&mut self, path: String, stage: Option<u32>) -> napi::Result<()> {
        let want: Option<u32> = stage;
        let mut prefix = path.as_bytes().to_vec();
        if !prefix.is_empty() && prefix.last() != Some(&b'/') {
            prefix.push(b'/');
        }
        let mut to_remove: Vec<usize> = Vec::new();
        for (i, e) in self.inner.entries().iter().enumerate() {
            if let Some(w) = want {
                if e.stage() as u32 != w {
                    continue;
                }
            }
            let p = e.path(&self.inner);
            if p.len() >= prefix.len() && &p[..prefix.len()] == prefix.as_slice() {
                to_remove.push(i);
            }
        }
        for i in to_remove.into_iter().rev() {
            self.inner.remove_entry_at_index(i);
        }
        Ok(())
    }

    /// Remove every entry whose path matches one of `pathspecs`, returning the number
    /// of entries removed. Matching is a simplified fnmatch: exact match, directory
    /// prefix (`spec/`), or a single `*` wildcard per spec.
    #[napi]
    pub fn remove_all(&mut self, pathspecs: Vec<String>) -> napi::Result<u32> {
        let mut to_remove: Vec<usize> = Vec::new();
        for (i, e) in self.inner.entries().iter().enumerate() {
            if pathspec_matches(e.path(&self.inner).as_ref(), &pathspecs) {
                to_remove.push(i);
            }
        }
        let removed = to_remove.len() as u32;
        for i in to_remove.into_iter().rev() {
            self.inner.remove_entry_at_index(i);
        }
        Ok(removed)
    }

    /// Whether the index contains any merge-conflict entries (stages 1–3).
    #[napi]
    pub fn has_conflicts(&self) -> bool {
        self.inner.entries().iter().any(|e| e.stage() as u32 != 0)
    }

    /// All merge conflicts in the index, grouped by path. Each conflict exposes its
    /// ancestor (stage 1), our (stage 2), and their (stage 3) sides, any of which may
    /// be `null`.
    #[napi]
    pub fn conflicts(&self) -> Vec<IndexConflict> {
        let mut map: BTreeMap<String, IndexConflict> = BTreeMap::new();
        for e in self.inner.entries() {
            let s = e.stage() as u32;
            if s == 0 {
                continue;
            }
            let path = lossy(e.path(&self.inner));
            let dto = entry_to_dto(&self.inner, e);
            let conflict = map.entry(path).or_insert(IndexConflict {
                ancestor: None,
                our: None,
                theirs: None,
            });
            match s {
                1 => conflict.ancestor = Some(dto),
                2 => conflict.our = Some(dto),
                3 => conflict.theirs = Some(dto),
                _ => {}
            }
        }
        map.into_values().collect()
    }

    /// Return the conflict at `path` (its three stages), or `null` when the path is not
    /// in conflict.
    #[napi]
    pub fn conflict_get(&self, path: String) -> Option<IndexConflict> {
        let mut conflict = IndexConflict {
            ancestor: None,
            our: None,
            theirs: None,
        };
        for s in [1u32, 2, 3] {
            let stage = stage_from_u32(s).ok()?;
            if let Some(e) = self.inner.entry_by_path_and_stage(bstr(&path), stage) {
                let dto = entry_to_dto(&self.inner, e);
                match s {
                    1 => conflict.ancestor = Some(dto),
                    2 => conflict.our = Some(dto),
                    3 => conflict.theirs = Some(dto),
                    _ => {}
                }
            }
        }
        if conflict.ancestor.is_none() && conflict.our.is_none() && conflict.theirs.is_none() {
            None
        } else {
            Some(conflict)
        }
    }

    /// Remove the conflict entries (stages 1–3) at `path`, keeping any stage-0 entry. A
    /// no-op when the path is not in conflict.
    #[napi]
    pub fn conflict_remove(&mut self, path: String) -> napi::Result<()> {
        for s in [1u32, 2, 3] {
            let stage = match stage_from_u32(s) {
                Ok(st) => st,
                Err(_) => continue,
            };
            while let Some(i) = self.inner.entry_index_by_path_and_stage(bstr(&path), stage) {
                self.inner.remove_entry_at_index(i);
            }
        }
        Ok(())
    }

    /// Return the position of the first entry whose path has `prefix` as a leading path
    /// component (i.e. `prefix == path`, or `path` starts with `prefix/`), or `null`
    /// when nothing matches. Mirrors libgit2's `git_index_find_prefix`.
    #[napi]
    pub fn find_prefix(&self, prefix: String) -> Option<u32> {
        let pre = prefix.as_bytes();
        if pre.is_empty() {
            return Some(0);
        }
        for (i, e) in self.inner.entries().iter().enumerate() {
            let p = e.path(&self.inner);
            if p == pre || (p.len() > pre.len() && &p[..pre.len()] == pre && p[pre.len()] == b'/') {
                return Some(i as u32);
            }
        }
        None
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
    pub fn index(&self) -> napi::Result<Index> {
        let r = thread_local(&self.inner);
        let shared = r.index().map_err(|e| err("index", e))?;
        Ok(Index {
            inner: gix::index::File::clone(&shared),
        })
    }

    /// Open a fresh, fully-decoded copy of the index file; fails if no index exists yet.
    /// With `skipHash`, the on-disk checksum verification is skipped (faster, less safe).
    #[napi]
    pub fn open_index(&self, skip_hash: Option<bool>) -> napi::Result<Index> {
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
    pub fn open_index_at(
        &self,
        path: String,
        options: Option<IndexOpenOptions>,
    ) -> napi::Result<Index> {
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
    pub fn open_index_file(&self) -> napi::Result<Index> {
        let r = thread_local(&self.inner);
        let file = r.open_index().map_err(|e| err("open_index", e))?;
        Ok(Index { inner: file })
    }

    /// Return the index if present, or `null` if the repository has no index file yet.
    #[napi]
    pub fn try_index(&self) -> napi::Result<Option<Index>> {
        let r = thread_local(&self.inner);
        let opt = r.try_index().map_err(|e| err("try_index", e))?;
        Ok(opt.map(|shared| Index {
            inner: gix::index::File::clone(&shared),
        }))
    }

    #[napi]
    pub fn index_or_empty(&self) -> napi::Result<Index> {
        let r = thread_local(&self.inner);
        let shared = r.index_or_empty().map_err(|e| err("index_or_empty", e))?;
        Ok(Index {
            inner: gix::index::File::clone(&shared),
        })
    }

    /// Return the index loaded from `HEAD^{tree}` if no index file exists, or an empty
    /// index if `HEAD` is unborn. Never fails due to a missing index.
    #[napi]
    pub fn index_or_load(&self) -> napi::Result<Index> {
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
    pub fn index_or_load_from_head(&self) -> napi::Result<Index> {
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
    pub fn index_from_tree(&self, tree: String) -> napi::Result<Index> {
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
    pub fn index_entries(&self) -> napi::Result<Vec<IndexEntry>> {
        Ok(self.open_index_file()?.entries())
    }

    /// Read entries from the index, or load them from `HEAD` / an empty index if absent.
    #[napi]
    pub fn index_or_load_from_head_entries(&self) -> napi::Result<Vec<IndexEntry>> {
        Ok(self.index_or_load()?.entries())
    }

    /// Add or update index entries in the repository index file.
    #[napi]
    pub fn edit_index(&self, entries: Vec<IndexEntryInput>) -> napi::Result<()> {
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

    /// Add (or replace) a single entry in `index` in memory, without touching the object
    /// database or the on-disk index file. The entry's `id` must already reference an
    /// object in this repository. Call `index.write()` to persist.
    #[napi]
    pub fn add_to_index(&self, index: &mut Index, entry: IndexEntryInput) -> napi::Result<()> {
        push_index_entry(&mut index.inner, &entry)
    }

    /// Write `data` to the object database as a blob, then add it to `index` in memory
    /// (the entry's own `id` is ignored in favor of the freshly-written blob). Returns
    /// the blob's object id. Call `index.write()` to persist the index.
    #[napi]
    pub fn add_from_buffer_to_index(
        &self,
        index: &mut Index,
        entry: IndexEntryInput,
        data: Buffer,
    ) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        let odb = r
            .objects
            .clone()
            .into_arc()
            .map_err(|e| err("add_from_buffer", e))?;
        let oid = gix::objs::Write::write_buf(&*odb, gix::object::Kind::Blob, data.as_ref())
            .map_err(|e| err("add_from_buffer", e))?;
        let mut entry = entry;
        entry.id = oid.to_string();
        push_index_entry(&mut index.inner, &entry)?;
        Ok(oid.to_string())
    }

    /// Recursively stage every file in the worktree into `index`, like
    /// `git add -A`. Ignored files are staged too (matching git_spawn's
    /// `Commit::commit`, which calls `index.add_all(["*"], …)` with no ignore
    /// callback). Entries that no longer exist on disk are removed, so the result
    /// matches the worktree exactly. Bare repositories are a no-op. The index is
    /// modified in memory; call `index.write()` to persist (or use
    /// `Repository.commitAll`, which writes a tree from the result).
    #[napi]
    pub fn add_all(&self, index: &mut Index) -> napi::Result<()> {
        let r = thread_local(&self.inner);
        let root = match r.worktree() {
            Some(wt) => wt.base().to_path_buf(),
            None => return Ok(()),
        };
        let mut present: Vec<Vec<u8>> = Vec::new();
        stage_dir(&root, &root, &r, &mut index.inner, &mut present)
            .map_err(|e| err("add_all", e))?;
        index
            .inner
            .remove_entries(|_i, path, _e| !present.iter().any(|p| p.as_slice() == path.as_bytes()));
        index.inner.sort_entries();
        index.inner.remove_tree();
        Ok(())
    }

    /// Build a git tree object from `index` (stage-0 entries only) and write it to this
    /// repository's object database, returning the tree's object id. Sparse indexes are
    /// not supported yet and produce an error.
    #[napi]
    pub fn write_tree_from_index(&self, index: &Index) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        let odb = r
            .objects
            .clone()
            .into_arc()
            .map_err(|e| err("write_tree", e))?;
        if index.inner.is_sparse() {
            return Err(err_str(
                "write_tree",
                "write_tree does not support sparse indexes yet",
            ));
        }
        let entries: Vec<(Vec<u8>, gix::index::entry::Mode, gix::ObjectId)> = index
            .inner
            .entries()
            .iter()
            .filter(|e| e.stage() as u32 == 0)
            .map(|e| (e.path(&index.inner).to_vec(), e.mode, e.id))
            .collect();
        let oid = write_tree_from_index_entries(&odb, &entries)?;
        Ok(oid.to_string())
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
    pub fn from_tree_mode(mode: String) -> napi::Result<Self> {
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
pub fn lstat(path: String) -> napi::Result<IndexFsMetadata> {
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
fn chunk_id(kind: &str) -> napi::Result<[u8; 4]> {
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
    pub fn from_bytes(data: Buffer, toc_offset: u32, num_chunks: u32) -> napi::Result<ChunkIndex> {
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
    pub fn offset_by_id(&self, kind: String) -> napi::Result<ChunkOffset> {
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
    pub fn data_by_id(&self, data: Buffer, kind: String) -> napi::Result<Buffer> {
        let id = chunk_id(&kind)?;
        let slice = self
            .inner
            .data_by_id(&data, id)
            .map_err(|e| err("chunk_index_data", e))?;
        Ok(Buffer::from(slice.to_vec()))
    }

    /// Enumerate all chunks as `{ kind, start, end }` by decoding the table of contents in `data`.
    #[napi]
    pub fn chunks(&self, data: Buffer) -> napi::Result<Vec<ChunkEntry>> {
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
    pub fn plan_chunk(&mut self, kind: String, exact_size_on_disk: i64) -> napi::Result<()> {
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
    pub fn write(&mut self, chunks: Vec<ChunkInput>) -> napi::Result<Buffer> {
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
