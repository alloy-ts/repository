use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use crate::error::RepositoryResult as Result;
use napi::Task;
use napi::bindgen_prelude::AsyncTask;
use napi::bindgen_prelude::Either;
use napi_derive::napi;

use gix::bstr::{BStr, BString};
use gix::diff::Rewrites;
use gix::diff::index::Change as TreeIndexChange;
use gix::remote::Direction;
use gix::status::index_worktree::Item as IndexWorktreeItem;
use gix::status::plumbing::index_as_worktree::{
    Change as WorktreeChange, Conflict as IndexConflict, EntryStatus as IndexEntryStatus,
};
use gix::status::tree_index::TrackRenames;
use gix::status::{Item as StatusItem, UntrackedFiles};

use crate::error::{err, err_str, thread_local};
use crate::repository::{Repository, lossy};

// ---------------------------------------------------------------------------
// JS-facing types
// ---------------------------------------------------------------------------

/// Options controlling how working tree status is computed
/// (mirrors `git status` / git2's `StatusOptions`).
#[napi(object)]
pub struct StatusOptions {
    /// Which sides to report: `"all"` (default, HEAD↔index↔worktree),
    /// `"index"` (HEAD→index only, staged) or `"worktree"` (index→worktree only).
    pub show: Option<String>,
    /// Untracked file listing: `"collapsed"` (default from config), `"files"`,
    /// or `"none"` (skip the directory walk entirely).
    pub untracked: Option<String>,
    /// Ignored file listing: `"none"` (default), `"matching"`, or `"collapsed"`.
    pub ignored: Option<String>,
    /// Restrict the report to paths matching these fnmatch-style patterns.
    pub pathspec: Option<Vec<String>>,
    /// Treat `pathspec` entries as literal paths, not patterns.
    pub disable_pathspec_match: Option<bool>,
    /// Skip all submodule status computation.
    pub exclude_submodules: Option<bool>,
    /// Diff the index against this tree-ish instead of `HEAD^{tree}`.
    pub head: Option<String>,
    /// Rename/copy detection between HEAD and the index:
    /// `"none"`, `"as-configured"` (default), or a [`RewritesOptions`] object.
    pub head_to_index_renames: Option<Either<String, RewritesOptions>>,
    /// Rename detection between index and worktree (no git config equivalent,
    /// default off like git): `false` (default), `true`, or [`RewritesOptions`].
    pub index_to_workdir_renames: Option<Either<bool, RewritesOptions>>,
    /// Write refreshed stat info back to the index for files that are
    /// unchanged but had stale stat data (git's soft index reload).
    pub update_index: Option<bool>,
}

/// Rename/copy detection tuning (like `git diff -M` / `-C`).
#[napi(object)]
pub struct RewritesOptions {
    /// Similarity threshold in percent (git `-M50%`), default `50`.
    pub percentage: Option<f64>,
    /// Cap on the number of rename candidates, default `1000` (git's default).
    pub limit: Option<f64>,
    /// Also track copies (`-C`).
    pub copies: Option<bool>,
}

fn opt_str_or_null(s: Option<String>) -> Either<String, napi::bindgen_prelude::Null> {
    match s {
        Some(val) => Either::A(val),
        None => Either::B(napi::bindgen_prelude::Null),
    }
}

/// A single status entry, one per changed path, merging the staged
/// (HEAD→index) and unstaged (index→worktree) sides.
#[derive(Clone)]
#[napi(object)]
pub struct StatusEntry {
    /// Current location of the entry (the rename destination, if renamed).
    pub path: String,
    /// Rename/copy source path, or `null`.
    pub old_path: Either<String, napi::bindgen_prelude::Null>,
    /// HEAD→index ("staged") change, or `null` when the index matches HEAD.
    pub index: Either<String, napi::bindgen_prelude::Null>,
    /// Index→worktree ("unstaged") change, or `null` when the worktree matches the index.
    pub worktree: Either<String, napi::bindgen_prelude::Null>,
    /// Two-letter `git status --porcelain` code, e.g. `" M"`, `"M "`, `"AM"`, `"R "`, `"??"`, `"UU"`.
    pub porcelain: String,
    /// `true` when the entry is a collapsed untracked/ignored *directory*.
    pub directory: bool,
    /// `true` when the entry is in a merge conflict (e.g. `UU`, `DD`, `AU`, ...).
    /// The `index`/`worktree` sides are `null` for conflicts, so this flag is the
    /// reliable way to detect them (mirrors `StatusKind::Unmerged`).
    pub conflict: bool,
    /// The specific kind of merge conflict (`DD`, `AU`, `UD`, `UA`, `DU`, `AA`,
    /// `UU`), or `null` when the entry is not conflicted. Mirrors `zerogit`'s
    /// `ConflictKind`; `porcelain` already carries the two-letter code.
    pub conflict_kind: Option<ConflictKind>,
    /// Coarse, single-value status for the path (one of `Untracked`, `Added`,
    /// `Modified`, `Deleted`, `StagedModified`, `StagedDeleted`, `Conflicted`),
    /// derived by merging the `index`/`worktree` sides. Convenience for callers
    /// that don't want to interpret both sides themselves.
    pub status: Option<FileStatus>,
    /// Object kind as stored in `HEAD` (e.g. `Regular`, `Executable`, `Symlink`,
    /// `Submodule`), or `null` when the path is not in `HEAD`.
    pub head_mode: Option<FileMode>,
    /// Object kind as stored in the index (stage 0), or `null` when the path is
    /// not in the index.
    pub index_mode: Option<FileMode>,
    /// Object kind as found in the work tree, or `null` when the file is absent
    /// from disk or its kind is unknown.
    pub worktree_mode: Option<FileMode>,
}

/// Aggregate working-tree status: the branch/tracking header plus all entries.
///
/// Returned by [`Repository::status`] (and the other status methods). `entries`
/// is one entry per changed path; `branch`/`tracking`/`ahead`/`behind` mirror
/// `git status -b`'s `## branch...tracking [ahead N, behind M]` header.
#[napi(object)]
pub struct Status {
    /// Current branch name, or `null` on a detached `HEAD`.
    pub branch: Option<String>,
    /// Upstream tracking ref (e.g. `origin/main`), or `null` with no upstream.
    pub tracking: Option<String>,
    /// Commits the local branch is ahead of `tracking` by.
    pub ahead: u32,
    /// Commits the local branch is behind `tracking` by.
    pub behind: u32,
    /// `true` when there are no changed entries in the requested scope.
    pub is_clean: bool,
    /// Per-path changes.
    pub entries: Vec<StatusEntry>,
}

pub use crate::conflict::ConflictKind;

impl ConflictKind {
    /// The two-letter porcelain code for this conflict.
    fn code(&self) -> &'static str {
        match self {
            ConflictKind::BothDeleted => "DD",
            ConflictKind::AddedByUs => "AU",
            ConflictKind::DeletedByThem => "UD",
            ConflictKind::AddedByThem => "UA",
            ConflictKind::DeletedByUs => "DU",
            ConflictKind::BothAdded => "AA",
            ConflictKind::BothModified => "UU",
        }
    }
}

/// The kind of a filesystem object as stored in a tree or the index.
/// Exposed on [`StatusEntry`]'s `headMode`/`indexMode`/`worktreeMode`.
#[napi(string_enum)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FileMode {
    /// A non-executable file (`100644`).
    Regular,
    /// An executable file (`100755`).
    Executable,
    /// A symbolic link (`120000`).
    Symlink,
    /// A submodule (a `commit` object, `160000`).
    Submodule,
    /// A directory / tree (`040000`).
    Directory,
}

impl FileMode {
    /// Map a gix tree-entry kind to our `FileMode`, or `None` for unknown kinds.
    fn from_kind(kind: gix::objs::tree::EntryKind) -> Option<FileMode> {
        Some(match kind {
            gix::objs::tree::EntryKind::Blob => FileMode::Regular,
            gix::objs::tree::EntryKind::BlobExecutable => FileMode::Executable,
            gix::objs::tree::EntryKind::Link => FileMode::Symlink,
            gix::objs::tree::EntryKind::Commit => FileMode::Submodule,
            gix::objs::tree::EntryKind::Tree => FileMode::Directory,
        })
    }
}

/// A coarse, single-value status for a path, merging the staged (`index`) and
/// unstaged (`worktree`) sides into one value (mirrors `zerogit`'s
/// `FileStatus`). Use this when you don't need the two-sided detail that
/// [`StatusEntry::index`] / [`StatusEntry::worktree`] provide.
#[napi(string_enum)]
#[derive(Clone, Copy)]
pub enum FileStatus {
    /// New file not tracked by Git.
    Untracked,
    /// Added to the index (staged for commit).
    Added,
    /// Modified in the working tree compared to the index.
    Modified,
    /// Deleted from the working tree.
    Deleted,
    /// Modified and staged.
    StagedModified,
    /// Deleted and staged.
    StagedDeleted,
    /// In an unresolved merge conflict.
    Conflicted,
}

// ---------------------------------------------------------------------------
// Internal configuration
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Show {
    All,
    Index,
    Worktree,
}

#[derive(Clone, Copy)]
enum IgnoredMode {
    Unspecified,
    None,
    Matching,
    Collapsed,
}

struct StatusCfg {
    show: Show,
    untracked: Option<UntrackedFiles>,
    ignored: IgnoredMode,
    pathspec: Vec<BString>,
    exclude_submodules: bool,
    head: Option<String>,
    h2i_renames: Option<TrackRenames>,
    i2w_rewrites: Option<Rewrites>,
    update_index: bool,
    pick: Option<BString>,
}

impl Default for StatusCfg {
    fn default() -> Self {
        StatusCfg {
            show: Show::All,
            untracked: None,
            ignored: IgnoredMode::Unspecified,
            pathspec: Vec::new(),
            exclude_submodules: false,
            head: None,
            h2i_renames: None,
            i2w_rewrites: None,
            update_index: false,
            pick: None,
        }
    }
}

fn parse_rewrites(o: &RewritesOptions) -> Rewrites {
    let mut r = Rewrites::default();
    if let Some(p) = o.percentage {
        r.percentage = Some((p as f32 / 100.0).clamp(0.0, 1.0));
    }
    if let Some(l) = o.limit {
        r.limit = l.max(0.0) as usize;
    }
    if o.copies.unwrap_or(false) {
        r.copies = Some(gix::diff::rewrites::Copies::default());
    }
    r
}

fn parse_show(s: Option<&str>) -> napi::Result<Show> {
    Ok(match s {
        None | Some("all") => Show::All,
        Some("index") => Show::Index,
        Some("worktree") => Show::Worktree,
        Some(other) => {
            return Err(err_str(
                "status options",
                format!(
                    "invalid show value {other:?}, expected \"all\", \"index\" or \"worktree\""
                ),
            )
            .into());
        }
    })
}

fn parse_untracked(s: Option<&str>) -> napi::Result<Option<UntrackedFiles>> {
    Ok(match s {
        None => None, // respect `status.showUntrackedFiles` config
        Some("collapsed") => Some(UntrackedFiles::Collapsed),
        Some("files") => Some(UntrackedFiles::Files),
        Some("none") => Some(UntrackedFiles::None),
        Some(other) => {
            return Err(err_str(
                "status options",
                format!(
                    "invalid untracked value {other:?}, expected \"collapsed\", \"files\" or \"none\""
                ),
            )
            .into());
        }
    })
}

fn parse_ignored(s: Option<&str>) -> napi::Result<IgnoredMode> {
    Ok(match s {
        None => IgnoredMode::Unspecified,
        Some("none") => IgnoredMode::None,
        Some("matching") => IgnoredMode::Matching,
        Some("collapsed") => IgnoredMode::Collapsed,
        Some(other) => {
            return Err(err_str(
                "status options",
                format!(
                    "invalid ignored value {other:?}, expected \"none\", \"matching\" or \"collapsed\""
                ),
            )
            .into());
        }
    })
}

fn parse_renames_str(s: &str) -> napi::Result<Option<TrackRenames>> {
    Ok(match s {
        "none" => Some(TrackRenames::Disabled),
        "as-configured" => Some(TrackRenames::AsConfigured),
        other => {
            return Err(err_str(
                "status options",
                format!(
                    "invalid headToIndexRenames value {other:?}, expected \"none\", \"as-configured\" or a RewritesOptions object"
                ),
            )
            .into());
        }
    })
}

fn parse_options(o: &StatusOptions) -> napi::Result<StatusCfg> {
    let mut cfg = StatusCfg {
        show: parse_show(o.show.as_deref())?,
        untracked: parse_untracked(o.untracked.as_deref())?,
        ignored: parse_ignored(o.ignored.as_deref())?,
        exclude_submodules: o.exclude_submodules.unwrap_or(false),
        head: o.head.clone(),
        h2i_renames: match &o.head_to_index_renames {
            None => None,
            Some(Either::A(s)) => parse_renames_str(s)?,
            Some(Either::B(r)) => Some(TrackRenames::Given(parse_rewrites(r))),
        },
        i2w_rewrites: match &o.index_to_workdir_renames {
            None => None,
            Some(Either::A(false)) => None,
            Some(Either::A(true)) => Some(Rewrites::default()),
            Some(Either::B(r)) => Some(parse_rewrites(r)),
        },
        update_index: o.update_index.unwrap_or(false),
        pick: None,
        pathspec: Vec::new(),
    };
    if let Some(patterns) = &o.pathspec {
        for p in patterns {
            if o.disable_pathspec_match.unwrap_or(false) {
                cfg.pathspec.push(format!(":(literal){p}").into());
            } else {
                cfg.pathspec.push(p.as_str().into());
            }
        }
    }
    Ok(cfg)
}

// ---------------------------------------------------------------------------
// Summary kinds & porcelain
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChange,
    Untracked,
    Ignored,
    IntentToAdd,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Added => "added",
            Kind::Modified => "modified",
            Kind::Deleted => "deleted",
            Kind::Renamed => "renamed",
            Kind::Copied => "copied",
            Kind::TypeChange => "typechange",
            Kind::Untracked => "untracked",
            Kind::Ignored => "ignored",
            Kind::IntentToAdd => "intent-to-add",
        }
    }

    fn letter(self) -> char {
        match self {
            Kind::Added | Kind::IntentToAdd => 'A',
            Kind::Modified => 'M',
            Kind::Deleted => 'D',
            Kind::Renamed => 'R',
            Kind::Copied => 'C',
            Kind::TypeChange => 'T',
            Kind::Untracked => '?',
            Kind::Ignored => '!',
        }
    }
}

fn conflict_kind_from(c: IndexConflict) -> ConflictKind {
    match c {
        IndexConflict::BothDeleted => ConflictKind::BothDeleted,
        IndexConflict::AddedByUs => ConflictKind::AddedByUs,
        IndexConflict::DeletedByThem => ConflictKind::DeletedByThem,
        IndexConflict::AddedByThem => ConflictKind::AddedByThem,
        IndexConflict::DeletedByUs => ConflictKind::DeletedByUs,
        IndexConflict::BothAdded => ConflictKind::BothAdded,
        IndexConflict::BothModified => ConflictKind::BothModified,
    }
}

/// One side of a status entry, before merging into the output map.
struct Half {
    path: BString,
    old_path: Option<BString>,
    index: Option<Kind>,
    worktree: Option<Kind>,
    conflict: Option<ConflictKind>,
    head_mode: Option<FileMode>,
    index_mode: Option<FileMode>,
    worktree_mode: Option<FileMode>,
    directory: bool,
}

fn mode_kind(mode: gix::index::entry::Mode) -> Option<FileMode> {
    mode.to_tree_entry_mode().and_then(|m| FileMode::from_kind(m.kind()))
}

/// Map a HEAD→index change into a `Half`.
fn half_from_tree_index(change: TreeIndexChange) -> Option<Half> {
    let (path, old_path, kind, head_mode, index_mode) = match change {
        TreeIndexChange::Addition {
            location,
            entry_mode,
            ..
        } => (location.into_owned(), None, Kind::Added, None, mode_kind(entry_mode)),
        TreeIndexChange::Deletion {
            location,
            entry_mode,
            ..
        } => (location.into_owned(), None, Kind::Deleted, mode_kind(entry_mode), None),
        TreeIndexChange::Modification {
            location,
            previous_entry_mode,
            entry_mode,
            ..
        } => {
            let kind = if mode_kind(previous_entry_mode) != mode_kind(entry_mode) {
                Kind::TypeChange
            } else {
                Kind::Modified
            };
            (
                location.into_owned(),
                None,
                kind,
                mode_kind(previous_entry_mode),
                mode_kind(entry_mode),
            )
        }
        TreeIndexChange::Rewrite {
            source_location,
            location,
            entry_mode,
            copy,
            ..
        } => (
            location.into_owned(),
            Some(source_location.into_owned()),
            if copy { Kind::Copied } else { Kind::Renamed },
            None,
            mode_kind(entry_mode),
        ),
    };
    Some(Half {
        path,
        old_path,
        index: Some(kind),
        worktree: None,
        conflict: None,
        head_mode,
        index_mode,
        worktree_mode: None,
        directory: false,
    })
}

fn dir_is_directory(entry: &gix::dir::Entry) -> bool {
    entry.disk_kind == Some(gix::dir::entry::Kind::Directory)
}

/// Map an index→worktree item into a `Half`.
fn half_from_index_worktree(item: IndexWorktreeItem) -> Option<Half> {
    match item {
        IndexWorktreeItem::Modification {
            entry,
            rela_path,
            status,
            ..
        } => {
            let index_mode = mode_kind(entry.mode);
            match status {
                IndexEntryStatus::Conflict { summary, .. } => Some(Half {
                    path: rela_path,
                    old_path: None,
                    index: None,
                    worktree: None,
                    conflict: Some(conflict_kind_from(summary)),
                    head_mode: None,
                    index_mode,
                    worktree_mode: None,
                    directory: false,
                }),
                IndexEntryStatus::Change(change) => {
                    let (kind, worktree_mode) = match change {
                        WorktreeChange::Removed => (Kind::Deleted, None),
                        WorktreeChange::Type { worktree_mode } => {
                            (Kind::TypeChange, mode_kind(worktree_mode))
                        }
                        WorktreeChange::Modification { .. } => (Kind::Modified, None),
                        WorktreeChange::SubmoduleModification(_) => (Kind::Modified, None),
                    };
                    Some(Half {
                        path: rela_path,
                        old_path: None,
                        index: None,
                        worktree: Some(kind),
                        conflict: None,
                        head_mode: None,
                        index_mode,
                        worktree_mode,
                        directory: false,
                    })
                }
                // Internal stat refresh; handled (and written back) via the outcome.
                IndexEntryStatus::NeedsUpdate(_) => None,
                IndexEntryStatus::IntentToAdd => Some(Half {
                    path: rela_path,
                    old_path: None,
                    index: None,
                    worktree: Some(Kind::IntentToAdd),
                    conflict: None,
                    head_mode: None,
                    index_mode,
                    worktree_mode: None,
                    directory: false,
                }),
            }
        }
        IndexWorktreeItem::DirectoryContents { entry, .. } => {
            let kind = match entry.status {
                gix::dir::entry::Status::Untracked => Kind::Untracked,
                gix::dir::entry::Status::Ignored(_) => Kind::Ignored,
                // Tracked and pruned entries are not status changes.
                _ => return None,
            };
            let directory = dir_is_directory(&entry);
            Some(Half {
                path: entry.rela_path,
                old_path: None,
                index: None,
                worktree: Some(kind),
                conflict: None,
                head_mode: None,
                index_mode: None,
                worktree_mode: None,
                directory,
            })
        }
        IndexWorktreeItem::Rewrite {
            source,
            dirwalk_entry,
            copy,
            ..
        } => {
            let directory = dir_is_directory(&dirwalk_entry);
            Some(Half {
                path: dirwalk_entry.rela_path,
                old_path: Some(source.rela_path().to_owned()),
                index: None,
                worktree: Some(if copy { Kind::Copied } else { Kind::Renamed }),
                conflict: None,
                head_mode: None,
                index_mode: None,
                worktree_mode: None,
                directory,
            })
        }
    }
}

fn porcelain(h: &Half) -> String {
    if let Some(kind) = h.conflict {
        return kind.code().to_string();
    }
    // Untracked / ignored entries are rendered as whole codes.
    if h.index.is_none() && h.worktree == Some(Kind::Untracked) {
        return "??".into();
    }
    if h.index.is_none() && h.worktree == Some(Kind::Ignored) {
        return "!!".into();
    }
    let x = h.index.map(|k| k.letter()).unwrap_or(' ');
    let y = h.worktree.map(|k| k.letter()).unwrap_or(' ');
    format!("{x}{y}")
}

/// Merge one half into the output map, keyed by path (deterministic, path-sorted).
fn merge(map: &mut BTreeMap<BString, Half>, half: Half) {
    let entry = map.entry(half.path.clone()).or_insert_with(|| Half {
        path: half.path.clone(),
        old_path: None,
        index: None,
        worktree: None,
        conflict: None,
        head_mode: None,
        index_mode: None,
        worktree_mode: None,
        directory: false,
    });
    if let Some(k) = half.index {
        // An intent-to-add file is not really staged; git renders it as " A".
        if k == Kind::Added && half.worktree == Some(Kind::IntentToAdd) {
            // fallthrough: cleared below
        }
        entry.index = Some(k);
    }
    if half.worktree == Some(Kind::IntentToAdd) && entry.worktree != Some(Kind::Modified) {
        // git status --porcelain shows intent-to-add as " A" with no staged side.
        entry.index = None;
    }
    if let Some(k) = half.worktree {
        entry.worktree = Some(k);
    }
    if let Some(c) = half.conflict {
        entry.conflict = Some(c);
    }
    entry.head_mode = half.head_mode.or(entry.head_mode);
    entry.index_mode = half.index_mode.or(entry.index_mode);
    entry.worktree_mode = half.worktree_mode.or(entry.worktree_mode);
    if half.old_path.is_some() {
        // The index half's oldPath wins for renames.
        entry.old_path = half.old_path;
    } else if entry.old_path.is_none() {
        entry.old_path = half.old_path;
    }
    entry.directory |= half.directory;
}

/// Derive a single coarse [`FileStatus`] from a two-sided [`Half`], following
/// the same reduction as `zerogit`'s `compute_status` (the staged side wins
/// when both sides changed).
fn coarse_status(h: &Half) -> Option<FileStatus> {
    if h.conflict.is_some() {
        return Some(FileStatus::Conflicted);
    }
    if h.index.is_none() && h.worktree == Some(Kind::Untracked) {
        return Some(FileStatus::Untracked);
    }
    if let Some(idx) = h.index {
        return Some(match idx {
            Kind::Added | Kind::IntentToAdd => FileStatus::Added,
            Kind::Modified | Kind::TypeChange | Kind::Renamed | Kind::Copied => {
                FileStatus::StagedModified
            }
            Kind::Deleted => FileStatus::StagedDeleted,
            Kind::Untracked | Kind::Ignored => FileStatus::Untracked,
        });
    }
    if let Some(wt) = h.worktree {
        return Some(match wt {
            Kind::Modified | Kind::TypeChange | Kind::Renamed | Kind::Copied => FileStatus::Modified,
            Kind::Deleted => FileStatus::Deleted,
            Kind::IntentToAdd => FileStatus::Added,
            Kind::Added | Kind::Untracked | Kind::Ignored => FileStatus::Untracked,
        });
    }
    None
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

/// Status computation task running on the libuv thread pool.
pub struct StatusTask {
    repo: Arc<gix::ThreadSafeRepository>,
    cfg: StatusCfg,
}

impl StatusTask {
    fn configure<'p, P>(
        &self,
        mut platform: gix::status::Platform<'p, P>,
        r: &gix::Repository,
    ) -> napi::Result<gix::status::Platform<'p, P>>
    where
        P: gix::progress::Progress + 'static,
    {
        if let Some(untracked) = self.cfg.untracked {
            platform = platform.untracked_files(untracked);
        }
        match self.cfg.ignored {
            IgnoredMode::Unspecified => {}
            IgnoredMode::None => {
                platform = platform.dirwalk_options(|o| o.emit_ignored(None));
            }
            IgnoredMode::Matching => {
                platform = platform.dirwalk_options(|o| {
                    o.emit_ignored(Some(gix::dir::walk::EmissionMode::Matching))
                });
            }
            IgnoredMode::Collapsed => {
                platform = platform.dirwalk_options(|o| {
                    o.emit_ignored(Some(gix::dir::walk::EmissionMode::CollapseDirectory))
                });
            }
        }
        if self.cfg.exclude_submodules {
            platform = platform.index_worktree_submodules(None);
        }
        if let Some(spec) = &self.cfg.head {
            let obj = r
                .rev_parse_single(spec.as_str())
                .map_err(|e| err("rev_parse_single", e))?;
            let object = obj
                .object()
                .map_err(|e| err("rev_parse_single object", e))?;
            let tree = object
                .peel_to_kind(gix::object::Kind::Tree)
                .map_err(|e| err("peel_to_kind tree", e))?;
            platform = platform.head_tree(tree.id);
        }
        if let Some(renames) = self.cfg.h2i_renames {
            platform = platform.tree_index_track_renames(renames);
        }
        if let Some(rewrites) = &self.cfg.i2w_rewrites {
            platform = platform.index_worktree_rewrites(rewrites.clone());
        }
        Ok(platform)
    }

    fn write_index(&self, outcome: Option<gix::status::Outcome>) -> napi::Result<()> {
        if !self.cfg.update_index {
            return Ok(());
        }
        if let Some(mut outcome) = outcome {
            if let Some(res) = outcome.write_changes() {
                res.map_err(|e| err("update index", e))?;
            }
        }
        Ok(())
    }

    fn finish(&self, map: BTreeMap<BString, Half>) -> Vec<StatusEntry> {
        map.into_values()
            .filter(
                |h| match (&self.cfg.show, h.index, h.worktree, h.conflict) {
                    // `show: "index"` drops the index→worktree-only halves.
                    (Show::Index, None, w, None) if w.is_some() => false,
                    _ => true,
                },
            )
            .map(|h| StatusEntry {
                path: lossy(&h.path),
                old_path: opt_str_or_null(h.old_path.as_deref().map(|p| lossy(p))),
                index: opt_str_or_null(h.index.map(|k| k.as_str().to_string())),
                worktree: opt_str_or_null(h.worktree.map(|k| k.as_str().to_string())),
                porcelain: porcelain(&h),
                directory: h.directory,
                conflict: h.conflict.is_some(),
                conflict_kind: h.conflict,
                status: coarse_status(&h),
                head_mode: h.head_mode,
                index_mode: h.index_mode,
                worktree_mode: h.worktree_mode,
            })
            .collect()
    }
}

impl StatusTask {
    fn compute_status(&self) -> napi::Result<Status> {
        let r = thread_local(&self.repo);
        let platform = r
            .status(gix::progress::Discard)
            .map_err(|e| err("status", e))?;
        let platform = self.configure(platform, &r)?;
        let mut map: BTreeMap<BString, Half> = BTreeMap::new();

        match self.cfg.show {
            Show::Worktree => {
                let mut iter = platform
                    .into_index_worktree_iter(self.cfg.pathspec.clone())
                    .map_err(|e| err("status iter", e))?;
                for item in iter.by_ref() {
                    let item = item.map_err(|e| err("status item", e))?;
                    if let Some(half) = half_from_index_worktree(item) {
                        merge(&mut map, half);
                    }
                }
                let outcome = iter.into_outcome();
                self.write_index(outcome)?;
            }
            Show::All | Show::Index => {
                let mut iter = platform
                    .into_iter(self.cfg.pathspec.clone())
                    .map_err(|e| err("status iter", e))?;
                for item in iter.by_ref() {
                    let item = item.map_err(|e| err("status item", e))?;
                    match item {
                        StatusItem::TreeIndex(change) => {
                            if let Some(half) = half_from_tree_index(change) {
                                merge(&mut map, half);
                            }
                        }
                        StatusItem::IndexWorktree(item) => {
                            if let Some(half) = half_from_index_worktree(item) {
                                merge(&mut map, half);
                            }
                        }
                    }
                }
                let outcome = iter.into_outcome();
                self.write_index(outcome)?;
            }
        }

        let mut entries = self.finish(map);
        if let Some(pick) = &self.cfg.pick {
            entries.retain(|e| e.path == lossy(pick));
        }
        let (branch, tracking, ahead, behind) = compute_header(&r);
        let is_clean = entries.is_empty();
        Ok(Status {
            branch,
            tracking,
            ahead,
            behind,
            is_clean,
            entries,
        })
    }
}

impl Task for StatusTask {
    type Output = Status;
    type JsValue = Status;

    fn compute(&mut self) -> napi::Result<Status> {
        self.compute_status()
    }

    fn resolve(&mut self, _env: napi::Env, output: Status) -> napi::Result<Status> {
        Ok(output)
    }
}

pub struct StatusEntriesTask {
    repo: Arc<gix::ThreadSafeRepository>,
    opts: Option<StatusOptions>,
    show_override: Option<Show>,
}

impl Task for StatusEntriesTask {
    type Output = Vec<StatusEntry>;
    type JsValue = Vec<StatusEntry>;

    fn compute(&mut self) -> napi::Result<Vec<StatusEntry>> {
        let mut cfg = match &self.opts {
            Some(o) => parse_options(o)?,
            None => StatusCfg::default(),
        };
        if let Some(s) = self.show_override {
            cfg.show = s;
        }
        let task = StatusTask {
            repo: Arc::clone(&self.repo),
            cfg,
        };
        let status = task.compute_status()?;
        Ok(status.entries)
    }

    fn resolve(&mut self, _env: napi::Env, output: Vec<StatusEntry>) -> napi::Result<Vec<StatusEntry>> {
        Ok(output)
    }
}

pub struct StatusFileTask {
    repo: Arc<gix::ThreadSafeRepository>,
    path: String,
}

impl Task for StatusFileTask {
    type Output = Option<StatusEntry>;
    type JsValue = Option<StatusEntry>;

    fn compute(&mut self) -> napi::Result<Option<StatusEntry>> {
        let cfg = StatusCfg {
            pathspec: vec![format!(":(literal){}", self.path).into()],
            pick: Some(self.path.clone().into_bytes().into()),
            ..StatusCfg::default()
        };
        let task = StatusTask {
            repo: Arc::clone(&self.repo),
            cfg,
        };
        let status = task.compute_status()?;
        Ok(status.entries.into_iter().next())
    }

    fn resolve(&mut self, _env: napi::Env, output: Option<StatusEntry>) -> napi::Result<Option<StatusEntry>> {
        Ok(output)
    }
}

// --- branch / tracking / ahead-behind ---------------------------------------

/// Compute the `git status -b` header for the current repository: branch name,
/// upstream tracking ref, and ahead/behind commit counts. Returns zeros when the
/// `HEAD` is detached or there is no configured upstream.
fn compute_header(r: &gix::Repository) -> (Option<String>, Option<String>, u32, u32) {
    let branch_full = match r.head_name() {
        Ok(Some(name)) => Some(name),
        _ => None,
    };
    let branch = branch_full.as_ref().map(|n| {
        let s = String::from_utf8_lossy(n.as_bstr());
        s.strip_prefix("refs/heads/").unwrap_or(&s).to_string()
    });

    let (tracking, ahead, behind) = match branch_full {
        Some(ref full) => {
            match r.branch_remote_tracking_ref_name(full.as_ref(), Direction::Fetch) {
                Some(Ok(tracking_full)) => {
                    let tracking_str = {
                        let s = String::from_utf8_lossy(tracking_full.as_bstr());
                        s.strip_prefix("refs/remotes/").unwrap_or(&s).to_string()
                    };
                    match tracking_commit_id(r, &tracking_full) {
                        Ok(upstream_id) => {
                            let (a, b) = ahead_behind(r, upstream_id).unwrap_or((0, 0));
                            (Some(tracking_str), a, b)
                        }
                        Err(_) => (Some(tracking_str), 0, 0),
                    }
                }
                _ => (None, 0, 0),
            }
        }
        None => (None, 0, 0),
    };
    (branch, tracking, ahead, behind)
}

/// Resolve the upstream tracking ref to its tip commit id.
fn tracking_commit_id(
    r: &gix::Repository,
    name: &gix::refs::FullName,
) -> napi::Result<gix::ObjectId> {
    let mut reference = r
        .find_reference(name)
        .map_err(|e| err("find_reference", e))?;
    let id = reference
        .peel_to_id()
        .map_err(|e| err("peel_to_id", e))?
        .detach();
    Ok(id)
}

/// Count commits the local `HEAD` is ahead of, and behind, `upstream_id`.
fn ahead_behind(r: &gix::Repository, upstream_id: gix::ObjectId) -> napi::Result<(u32, u32)> {
    let head = match r.head_commit() {
        Ok(c) => c,
        Err(_) => return Ok((0, 0)),
    };
    let head_id = head.id;
    let base = match r
        .merge_base(head_id, upstream_id)
        .map_err(|e| err("merge_base", e))?
    {
        Some(b) => b.detach(),
        None => {
            let ahead = count_from(r, head_id, None)?;
            let behind = count_from(r, upstream_id, None)?;
            return Ok((ahead, behind));
        }
    };
    let ahead = count_from(r, head_id, Some(base))?;
    let behind = count_from(r, upstream_id, Some(base))?;
    Ok((ahead, behind))
}

/// Count commits reachable from `from`, stopping (exclusively) at `stop`.
fn count_from(
    r: &gix::Repository,
    from: gix::ObjectId,
    stop: Option<gix::ObjectId>,
) -> napi::Result<u32> {
    let commit = r.find_commit(from).map_err(|e| err("find_commit", e))?;
    let walk = commit
        .ancestors()
        .selected(move |oid| match stop {
            Some(s) => oid.as_bytes() != s.as_bytes(),
            None => true,
        })
        .map_err(|e| err("ancestors", e))?;
    Ok(walk.filter_map(|r| r.ok()).count() as u32)
}

/// Single-path dirty probe running on the libuv thread pool.
pub struct IsDirtyTask {
    repo: Arc<gix::ThreadSafeRepository>,
}

impl Task for IsDirtyTask {
    type Output = bool;
    type JsValue = bool;

    fn compute(&mut self) -> napi::Result<bool> {
        let r = thread_local(&self.repo);
        Ok(r.is_dirty().map_err(|e| err("is_dirty", e))?)
    }

    fn resolve(&mut self, _env: napi::Env, output: bool) -> napi::Result<bool> {
        Ok(output)
    }
}

// ---------------------------------------------------------------------------
// NAPI methods
// ---------------------------------------------------------------------------

#[napi]
impl Repository {
    /// Compute working tree status entries asynchronously. Returns an array of [`StatusEntry`]
    /// elements, one per changed path.
    #[napi(ts_return_type = "Promise<StatusEntry[]>")]
    pub fn status(
        &self,
        options: Option<StatusOptions>,
    ) -> napi::Result<AsyncTask<StatusEntriesTask>> {
        Ok(AsyncTask::new(StatusEntriesTask {
            repo: Arc::clone(&self.inner),
            opts: options,
            show_override: None,
        }))
    }

    /// Compute the full working tree status asynchronously with branch and tracking header info.
    #[napi(ts_return_type = "Promise<Status>")]
    pub fn status_detailed(
        &self,
        options: Option<StatusOptions>,
    ) -> napi::Result<AsyncTask<StatusTask>> {
        let cfg = match &options {
            Some(o) => parse_options(o)?,
            None => StatusCfg::default(),
        };
        Ok(AsyncTask::new(StatusTask {
            repo: Arc::clone(&self.inner),
            cfg,
        }))
    }

    /// Compute the index↔worktree status entries asynchronously (never touches HEAD;
    /// returned entries always have `index == null`).
    #[napi(ts_return_type = "Promise<StatusEntry[]>")]
    pub fn index_worktree(
        &self,
        options: Option<StatusOptions>,
    ) -> napi::Result<AsyncTask<StatusEntriesTask>> {
        Ok(AsyncTask::new(StatusEntriesTask {
            repo: Arc::clone(&self.inner),
            opts: options,
            show_override: Some(Show::Worktree),
        }))
    }

    /// Returns `true` when the repository has staged or unstaged changes.
    /// Untracked files do **not** count, matching `git status` semantics.
    #[napi(ts_return_type = "Promise<boolean>")]
    pub fn is_dirty(&self) -> napi::Result<AsyncTask<IsDirtyTask>> {
        Ok(AsyncTask::new(IsDirtyTask {
            repo: Arc::clone(&self.inner),
        }))
    }

    /// Compute the status of a single path asynchronously, returning the [`StatusEntry`]
    /// or `null` when clean/unknown.
    #[napi(ts_return_type = "Promise<StatusEntry | null>")]
    pub fn status_file(&self, path: String) -> napi::Result<AsyncTask<StatusFileTask>> {
        Ok(AsyncTask::new(StatusFileTask {
            repo: Arc::clone(&self.inner),
            path,
        }))
    }

    /// Check synchronously whether a path is excluded by `.gitignore` rules.
    #[napi]
    pub fn should_ignore(&self, path: String) -> napi::Result<bool> {
        let r = thread_local(&self.inner);
        let index = r.index_or_empty().map_err(|e| err("index", e))?;
        let mut stack = r
            .excludes(
                &index,
                None,
                gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
            )
            .map_err(|e| err("excludes", e))?;
        let platform = stack
            .at_path(Path::new(path.as_str()), None)
            .map_err(|e| err("at_path", e))?;
        Ok(platform.is_excluded())
    }
}

// Keep unused-import lints quiet for types only referenced by feature-gated code.
#[allow(unused_imports)]
use gix::bstr::ByteSlice as _;

/// Convert a `BStr` path for `statusFile` matching (`BString` → owned bytes).
trait PathPick {
    fn from_bytes(b: &[u8]) -> Self;
}

impl PathPick for BString {
    fn from_bytes(b: &[u8]) -> Self {
        b.to_vec().into()
    }
}

/// Reference to a borrowed byte path, used by [`Repository::status_file`].
#[allow(dead_code)]
fn as_bstr(b: &[u8]) -> &BStr {
    BStr::new(b)
}
