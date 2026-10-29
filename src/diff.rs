use napi::bindgen_prelude::AsyncTask;
use napi::{Env, Task};

use napi_derive::napi;
use std::sync::Arc;

use crate::error::{err, parse_oid, thread_local};
use crate::repository::{Repository, lossy};
use crate::rev_walk::WalkOptions;
use gix::bstr::BString;
use gix_imara_diff as imara;

/// A single change produced by the `diff*` methods.
#[derive(Clone)]
#[napi(object)]
pub struct Change {
    /// Repo-relative path of the change. For renames/copies this is the *destination* path.
    pub path: String,
    /// Source path for renames/copies; `None` for additions, modifications, and deletions.
    pub source_location: Option<String>,
    pub old_id: Option<String>,
    pub new_id: Option<String>,
    /// The kind of change: `Added`, `Modified`, `Deleted`, `Renamed`, `Copied`,
    /// `TypeChanged`, `Conflict`, `IntentToAdd` or `Ignored`.
    pub kind: String,
    /// Git mode of the entry after the change, as an octal string (`100644`, `100755`, `040000`, `120000`).
    pub entry_mode: String,
    /// Git mode of the entry before the change (modifications and renames/copies only); `None` otherwise.
    pub previous_entry_mode: Option<String>,
}

fn mode_string(mode: gix::object::tree::EntryMode) -> String {
    format!("{:06o}", mode.value())
}

/// Format an index entry mode as an octal git mode string.
fn index_mode_string(mode: gix::index::entry::Mode) -> String {
    match mode.to_tree_entry_mode() {
        Some(m) => mode_string(m),
        None => "000000".to_string(),
    }
}

/// Map a directory-walk entry kind to an octal git mode string.
fn dirwalk_kind_string(kind: gix::dir::entry::Kind) -> Option<String> {
    Some(match kind {
        gix::dir::entry::Kind::File => "100644".to_string(),
        gix::dir::entry::Kind::Symlink => "120000".to_string(),
        gix::dir::entry::Kind::Directory => "040000".to_string(),
        gix::dir::entry::Kind::Repository => "160000".to_string(),
        gix::dir::entry::Kind::Untrackable => return None,
    })
}

/// Aggregate line/change statistics for a tree-to-tree diff (mirrors `git diff --stat`).
#[derive(Clone)]
#[napi(object)]
pub struct DiffStats {
    /// Number of files added, removed, or modified.
    pub files_changed: f64,
    /// Total number of inserted lines.
    pub lines_added: f64,
    /// Total number of removed lines.
    pub lines_removed: f64,
}

/// A typed summary of the working tree and index relative to `HEAD` and the
/// upstream branch — a single `git status` rolled into one object.
///
/// Produced by [`Repository::changes`]. The three path lists are disjoint by
/// origin, not by file: a path changed in both the index and the worktree appears
/// in both [`staged`](Changes::staged) and [`unstaged`](Changes::unstaged).
#[derive(Clone)]
#[napi(object)]
pub struct Changes {
    /// Current branch name (short, e.g. `main`). `null` for a detached `HEAD`.
    pub branch: Option<String>,
    /// Upstream tracking ref (e.g. `refs/remotes/origin/main`), if one is configured.
    pub tracking: Option<String>,
    /// Commits the local branch is ahead of `tracking` by.
    pub ahead: u32,
    /// Commits the local branch is behind `tracking` by.
    pub behind: u32,
    /// Paths with staged changes (a non-empty index column).
    pub staged: Vec<String>,
    /// Paths with unstaged changes to tracked files.
    pub unstaged: Vec<String>,
    /// Untracked paths (porcelain `??`); ignored paths are excluded.
    pub untracked: Vec<String>,
    /// `true` when there are no staged, unstaged, or untracked changes.
    pub is_clean: bool,
    /// `true` when there is at least one staged, unstaged, or untracked change.
    pub is_dirty: bool,
}

/// Rename and copy detection mode.
#[derive(Clone, Copy)]
#[napi(string_enum)]
pub enum TrackRenames {
    /// Follow repository configuration (`diff.renames` / `status.renames`); renames are
    /// detected at 50% similarity by default. This is the default.
    AsConfigured,
    /// Do not detect renames or copies.
    Disabled,
    /// Use the explicit `renameThreshold`, `renameLimit`, `trackEmptyFiles` and `findCopies`
    /// settings on the diff options object.
    Custom,
}

fn build_rewrites(
    track: Option<TrackRenames>,
    threshold: Option<f64>,
    limit: Option<f64>,
    track_empty: Option<bool>,
    find_copies: Option<bool>,
) -> Option<gix::diff::Rewrites> {
    if !matches!(track, Some(TrackRenames::Custom)) {
        return None;
    }
    Some(gix::diff::Rewrites {
        copies: find_copies
            .unwrap_or(false)
            .then(gix::diff::rewrites::Copies::default),
        percentage: threshold.map(|t| (t / 100.0).clamp(0.0, 1.0) as f32),
        limit: limit.map(|l| l as usize).unwrap_or(1000),
        track_empty: track_empty.unwrap_or(false),
    })
}

/// Options controlling tree-to-tree and tree-to-index diffs.
#[derive(Clone)]
#[napi(object)]
pub struct DiffOptions {
    /// Rename/copy detection mode (default `AsConfigured`).
    pub track_renames: Option<TrackRenames>,
    /// Similarity (0-100) needed to consider files renamed or copied (git's `-M<n>`).
    /// Only applied when `trackRenames` is `Custom`; when omitted, only 100% content
    /// matches are considered.
    pub rename_threshold: Option<f64>,
    /// Maximum number of similarity candidates examined per changed file
    /// (git's `-l` / `diff.renameLimit`). `0` means unlimited.
    /// Only applied when `trackRenames` is `Custom`.
    pub rename_limit: Option<f64>,
    /// Include empty files in rename tracking.
    /// Only applied when `trackRenames` is `Custom`.
    pub track_empty_files: Option<bool>,
    /// Also detect copies (git's `--find-copies`).
    /// Only applied when `trackRenames` is `Custom`.
    pub find_copies: Option<bool>,
    /// Limit the diff to these paths (git pathspec syntax). Not supported by
    /// [`Repository::diff_tree_to_tree`] — gix's tree diff has no pathspec filter.
    pub pathspec: Option<Vec<String>>,
}

/// How untracked files are reported by worktree diffs.
#[derive(Clone, Copy)]
#[napi(string_enum)]
pub enum UntrackedMode {
    /// Omit untracked files entirely.
    None,
    /// Collapse untracked directories into a single entry (the default).
    Collapsed,
    /// Report every untracked file individually.
    Files,
}

/// How submodule status is computed for worktree diffs.
#[derive(Clone, Copy)]
#[napi(string_enum)]
pub enum SubmoduleMode {
    /// Follow configuration (the default).
    AsConfigured,
    /// Skip submodules entirely.
    Ignore,
    /// Only report submodules as added/removed, without inspecting their contents.
    Untracked,
    /// Stop at the first sign of submodule dirtiness (faster, less precise).
    Dirty,
}

/// Options controlling index-to-worktree and tree-to-worktree diffs.
#[derive(Clone)]
#[napi(object)]
pub struct WorktreeDiffOptions {
    /// Rename/copy detection mode (default `AsConfigured`; only the tree-to-index
    /// portion supports `Custom` settings).
    pub track_renames: Option<TrackRenames>,
    /// See [`DiffOptions::rename_threshold`].
    pub rename_threshold: Option<f64>,
    /// See [`DiffOptions::rename_limit`].
    pub rename_limit: Option<f64>,
    /// See [`DiffOptions::track_empty_files`].
    pub track_empty_files: Option<bool>,
    /// See [`DiffOptions::find_copies`].
    pub find_copies: Option<bool>,
    /// Limit the diff to these paths (git pathspec syntax).
    pub pathspec: Option<Vec<String>>,
    /// How to report untracked files (default `Collapsed`).
    pub untracked: Option<UntrackedMode>,
    /// How much work to invest in submodule status (default `AsConfigured`).
    pub submodules: Option<SubmoduleMode>,
}

/// Resolved settings for the worktree-diff entry points.
type WorktreeSettings = (
    Option<gix::diff::Rewrites>,
    Vec<BString>,
    gix::status::UntrackedFiles,
    gix::status::Submodule,
    gix::status::tree_index::TrackRenames,
);

fn worktree_settings(options: Option<&WorktreeDiffOptions>) -> WorktreeSettings {
    let track = options
        .and_then(|o| o.track_renames)
        .unwrap_or(TrackRenames::AsConfigured);
    let rewrites = options.and_then(|o| {
        build_rewrites(
            o.track_renames,
            o.rename_threshold,
            o.rename_limit,
            o.track_empty_files,
            o.find_copies,
        )
    });
    let tree_renames = match track {
        TrackRenames::AsConfigured => gix::status::tree_index::TrackRenames::AsConfigured,
        TrackRenames::Disabled => gix::status::tree_index::TrackRenames::Disabled,
        TrackRenames::Custom => gix::status::tree_index::TrackRenames::Given(
            options
                .and_then(|o| {
                    build_rewrites(
                        o.track_renames,
                        o.rename_threshold,
                        o.rename_limit,
                        o.track_empty_files,
                        o.find_copies,
                    )
                })
                .unwrap_or_default(),
        ),
    };
    let untracked = match options.and_then(|o| o.untracked) {
        Some(UntrackedMode::None) => gix::status::UntrackedFiles::None,
        Some(UntrackedMode::Files) => gix::status::UntrackedFiles::Files,
        _ => gix::status::UntrackedFiles::Collapsed,
    };
    let submodule = match options.and_then(|o| o.submodules) {
        Some(SubmoduleMode::Ignore) => gix::status::Submodule::Given {
            ignore: gix::submodule::config::Ignore::All,
            check_dirty: false,
        },
        Some(SubmoduleMode::Untracked) => gix::status::Submodule::Given {
            ignore: gix::submodule::config::Ignore::Untracked,
            check_dirty: false,
        },
        Some(SubmoduleMode::Dirty) => gix::status::Submodule::Given {
            ignore: gix::submodule::config::Ignore::Dirty,
            check_dirty: false,
        },
        _ => gix::status::Submodule::AsConfigured { check_dirty: false },
    };
    let patterns = options
        .and_then(|o| o.pathspec.as_ref())
        .map(|p| {
            p.iter()
                .map(|s| BString::from(s.as_bytes()))
                .collect::<Vec<BString>>()
        })
        .unwrap_or_default();
    (rewrites, patterns, untracked, submodule, tree_renames)
}

// ---------------------------------------------------------------------------
// Shared change mapping (single source of truth for sync + async variants)
// ---------------------------------------------------------------------------

fn map_tree_change(change: gix::object::tree::diff::ChangeDetached) -> Change {
    match change {
        gix::object::tree::diff::ChangeDetached::Addition {
            location,
            id,
            entry_mode,
            ..
        } => Change {
            path: lossy(&*location),
            source_location: None,
            old_id: None,
            new_id: Some(id.to_string()),
            kind: "Added".to_string(),
            entry_mode: mode_string(entry_mode),
            previous_entry_mode: None,
        },
        gix::object::tree::diff::ChangeDetached::Deletion {
            location,
            id,
            entry_mode,
            ..
        } => Change {
            path: lossy(&*location),
            source_location: None,
            old_id: Some(id.to_string()),
            new_id: None,
            kind: "Deleted".to_string(),
            entry_mode: mode_string(entry_mode),
            previous_entry_mode: None,
        },
        gix::object::tree::diff::ChangeDetached::Modification {
            location,
            previous_id,
            previous_entry_mode,
            id,
            entry_mode,
            ..
        } => Change {
            path: lossy(&*location),
            source_location: None,
            old_id: Some(previous_id.to_string()),
            new_id: Some(id.to_string()),
            kind: "Modified".to_string(),
            entry_mode: mode_string(entry_mode),
            previous_entry_mode: Some(mode_string(previous_entry_mode)),
        },
        gix::object::tree::diff::ChangeDetached::Rewrite {
            location,
            source_location,
            source_entry_mode,
            source_id,
            entry_mode,
            id,
            copy,
            ..
        } => Change {
            path: lossy(&*location),
            source_location: Some(lossy(&*source_location)),
            old_id: Some(source_id.to_string()),
            new_id: Some(id.to_string()),
            kind: if copy { "Copied" } else { "Renamed" }.to_string(),
            entry_mode: mode_string(entry_mode),
            previous_entry_mode: Some(mode_string(source_entry_mode)),
        },
    }
}

/// Map a tree↔index change. Accepts both the borrowed `ChangeRef` and the owned
/// `Change` (a `ChangeRef<'static, 'static>` alias), so tree-to-index and
/// tree-to-worktree diffs share one mapping.
fn map_index_change(change: gix::diff::index::ChangeRef<'_, '_>) -> Change {
    use gix::diff::index::ChangeRef as C;
    match change {
        C::Addition {
            location,
            entry_mode,
            id,
            ..
        } => Change {
            path: lossy(&*location),
            source_location: None,
            old_id: None,
            new_id: Some(id.into_owned().to_string()),
            kind: "Added".to_string(),
            entry_mode: index_mode_string(entry_mode),
            previous_entry_mode: None,
        },
        C::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => Change {
            path: lossy(&*location),
            source_location: None,
            old_id: Some(id.into_owned().to_string()),
            new_id: None,
            kind: "Deleted".to_string(),
            entry_mode: index_mode_string(entry_mode),
            previous_entry_mode: None,
        },
        C::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
            ..
        } => Change {
            path: lossy(&*location),
            source_location: None,
            old_id: Some(previous_id.into_owned().to_string()),
            new_id: Some(id.into_owned().to_string()),
            kind: "Modified".to_string(),
            entry_mode: index_mode_string(entry_mode),
            previous_entry_mode: Some(index_mode_string(previous_entry_mode)),
        },
        C::Rewrite {
            source_location,
            source_entry_mode,
            source_id,
            location,
            entry_mode,
            id,
            copy,
            ..
        } => Change {
            path: lossy(&*location),
            source_location: Some(lossy(&*source_location)),
            old_id: Some(source_id.into_owned().to_string()),
            new_id: Some(id.into_owned().to_string()),
            kind: if copy { "Copied" } else { "Renamed" }.to_string(),
            entry_mode: index_mode_string(entry_mode),
            previous_entry_mode: Some(index_mode_string(source_entry_mode)),
        },
    }
}

/// Map an index↔worktree item to a [`Change`]; `None` for items that carry no user-visible
/// change (index stat refreshes, tracked/ignored directory contents).
fn map_worktree_item(item: gix::status::index_worktree::Item) -> Option<Change> {
    use gix::status::index_worktree::{Item as W, RewriteSource as RS};
    use gix::status::plumbing::index_as_worktree::{Change as WChange, EntryStatus};
    match item {
        W::Modification {
            entry,
            rela_path,
            status,
            ..
        } => {
            let kind = match &status {
                EntryStatus::Conflict { .. } => "Conflict",
                EntryStatus::Change(WChange::Removed) => "Deleted",
                EntryStatus::Change(WChange::Type { .. }) => "TypeChanged",
                EntryStatus::Change(WChange::Modification { .. })
                | EntryStatus::Change(WChange::SubmoduleModification(_)) => "Modified",
                EntryStatus::NeedsUpdate(_) => return None,
                EntryStatus::IntentToAdd => "IntentToAdd",
            };
            Some(Change {
                path: lossy(&rela_path),
                source_location: None,
                old_id: Some(entry.id.to_string()),
                new_id: None,
                kind: kind.to_string(),
                entry_mode: index_mode_string(entry.mode),
                previous_entry_mode: None,
            })
        }
        W::DirectoryContents { entry, .. } => {
            let kind = match entry.status {
                gix::dir::entry::Status::Untracked => "Added",
                gix::dir::entry::Status::Ignored(_) => "Ignored",
                _ => return None,
            };
            let mode = entry
                .disk_kind
                .or(entry.index_kind)
                .and_then(dirwalk_kind_string);
            Some(Change {
                path: lossy(&entry.rela_path),
                source_location: None,
                old_id: None,
                new_id: None,
                kind: kind.to_string(),
                entry_mode: mode.unwrap_or_else(|| "100644".to_string()),
                previous_entry_mode: None,
            })
        }
        W::Rewrite {
            source,
            dirwalk_entry,
            dirwalk_entry_id,
            copy,
            ..
        } => {
            let (source_location, source_id, previous_entry_mode) = match &source {
                RS::RewriteFromIndex {
                    source_entry,
                    source_rela_path,
                    ..
                } => (
                    lossy(source_rela_path),
                    Some(source_entry.id.to_string()),
                    Some(index_mode_string(source_entry.mode)),
                ),
                RS::CopyFromDirectoryEntry {
                    source_dirwalk_entry,
                    source_dirwalk_entry_id,
                    ..
                } => (
                    lossy(&source_dirwalk_entry.rela_path),
                    Some(source_dirwalk_entry_id.to_string()),
                    source_dirwalk_entry
                        .disk_kind
                        .or(source_dirwalk_entry.index_kind)
                        .and_then(dirwalk_kind_string),
                ),
            };
            let mode = dirwalk_entry
                .disk_kind
                .or(dirwalk_entry.index_kind)
                .and_then(dirwalk_kind_string);
            Some(Change {
                path: lossy(&dirwalk_entry.rela_path),
                source_location: Some(source_location),
                old_id: source_id,
                new_id: if dirwalk_entry_id.is_null() {
                    None
                } else {
                    Some(dirwalk_entry_id.to_string())
                },
                kind: if copy { "Copied" } else { "Renamed" }.to_string(),
                entry_mode: mode.unwrap_or_else(|| "100644".to_string()),
                previous_entry_mode,
            })
        }
    }
}

/// Resolve a tree argument (`None` = the HEAD tree).
fn resolve_tree_id(r: &gix::Repository, tree: Option<&str>) -> napi::Result<gix::hash::ObjectId> {
    match tree {
        Some(t) => Ok(parse_oid(t)?),
        None => Ok(r.head_tree_id().map_err(|e| err("head_tree", e))?.detach()),
    }
}

// ---------------------------------------------------------------------------
// Shared compute paths (single source of truth for sync + async variants)
// ---------------------------------------------------------------------------

fn compute_tree_diff(
    r: &gix::Repository,
    old: Option<&str>,
    new_id: Option<&str>,
    options: Option<&DiffOptions>,
) -> napi::Result<Vec<Change>> {
    let empty = gix::ObjectId::empty_tree(r.object_hash());
    let old_tree = old
        .map(|o| r.find_tree(parse_oid(o)?).map_err(|e| err("find_tree", e)))
        .transpose()?;
    let new_tree = match new_id {
        Some(n) => r
            .find_tree(parse_oid(n)?)
            .map_err(|e| err("find_tree", e))?,
        None => r.find_tree(empty).map_err(|e| err("find_tree", e))?,
    };
    // `None` => gix follows repository configuration (`diff.renames`).
    // `Some(Options::default())` => rename tracking off.
    // `Some(Options { rewrites: Some(…) })` => explicit settings.
    let gix_options = match options {
        None => None,
        Some(o) => match o.track_renames {
            None | Some(TrackRenames::AsConfigured) => None,
            Some(TrackRenames::Disabled) => Some(gix::diff::Options::default()),
            Some(TrackRenames::Custom) => {
                let mut opts = gix::diff::Options::default();
                opts.track_rewrites(build_rewrites(
                    o.track_renames,
                    o.rename_threshold,
                    o.rename_limit,
                    o.track_empty_files,
                    o.find_copies,
                ));
                Some(opts)
            }
        },
    };
    let changes = r
        .diff_tree_to_tree(old_tree.as_ref(), Some(&new_tree), gix_options)
        .map_err(|e| err("diff_tree_to_tree", e))?;
    Ok(changes.into_iter().map(map_tree_change).collect())
}

/// Tree↔index changes via `tree_index_status`, shared by `diffTreeToIndex` and
/// the tree↔index half of `diffTreeToWorkdir`.
fn tree_index_changes(
    r: &gix::Repository,
    tree_id: &gix::hash::ObjectId,
    renames: gix::status::tree_index::TrackRenames,
    patterns: &[BString],
) -> napi::Result<Vec<Change>> {
    let index_shared = r.index_or_empty().map_err(|e| err("index", e))?;
    let index: &gix::index::State = &index_shared;
    // Optional pathspec; gix builds an internal default pathspec when `None` is passed.
    let mut pathspec_storage;
    let pathspec = if patterns.is_empty() {
        None
    } else {
        let empty_index = gix::index::State::new(r.object_hash());
        pathspec_storage = Some(
            r.pathspec(
                true,
                patterns.iter(),
                false,
                &empty_index,
                gix::worktree::stack::state::attributes::Source::IdMapping,
            )
            .map_err(|e| err("pathspec", e))?,
        );
        pathspec_storage.as_mut()
    };
    let mut out = Vec::new();
    r.tree_index_status(tree_id, index, pathspec, renames, |change, _lhs, _rhs| {
        out.push(map_index_change(change));
        Ok(std::ops::ControlFlow::Continue(()))
    })
    .map_err(|e| err("tree_index_status", e))?;
    Ok(out)
}

/// Index↔worktree changes via the status platform, shared by `diffIndexToWorkdir` and
/// the index↔worktree half of `diffTreeToWorkdir`.
fn index_worktree_changes(
    r: &gix::Repository,
    rewrites: Option<gix::diff::Rewrites>,
    patterns: Vec<BString>,
    untracked: gix::status::UntrackedFiles,
    submodule: gix::status::Submodule,
) -> napi::Result<Vec<Change>> {
    let platform = r
        .status(gix::progress::Discard)
        .map_err(|e| err("status", e))?
        .untracked_files(untracked)
        .index_worktree_submodules(submodule);
    let platform = match rewrites {
        Some(rw) => platform.index_worktree_rewrites(Some(rw)),
        None => platform,
    };
    let iter = platform
        .into_index_worktree_iter(patterns)
        .map_err(|e| err("status_iter", e))?;
    let mut out = Vec::new();
    for item in iter {
        let item = item.map_err(|e| err("status_item", e))?;
        if let Some(change) = map_worktree_item(item) {
            out.push(change);
        }
    }
    Ok(out)
}

fn compute_tree_index_diff(
    r: &gix::Repository,
    tree: Option<&str>,
    options: Option<&DiffOptions>,
) -> napi::Result<Vec<Change>> {
    let tree_id = resolve_tree_id(r, tree)?;
    let renames = match options.and_then(|o| o.track_renames) {
        Some(TrackRenames::Disabled) => gix::status::tree_index::TrackRenames::Disabled,
        Some(TrackRenames::Custom) => gix::status::tree_index::TrackRenames::Given(
            options
                .and_then(|o| {
                    build_rewrites(
                        o.track_renames,
                        o.rename_threshold,
                        o.rename_limit,
                        o.track_empty_files,
                        o.find_copies,
                    )
                })
                .unwrap_or_default(),
        ),
        _ => gix::status::tree_index::TrackRenames::AsConfigured,
    };
    let patterns = options
        .and_then(|o| o.pathspec.as_ref())
        .map(|p| {
            p.iter()
                .map(|s| BString::from(s.as_bytes()))
                .collect::<Vec<BString>>()
        })
        .unwrap_or_default();
    tree_index_changes(r, &tree_id, renames, &patterns)
}

fn compute_index_worktree_diff(
    r: &gix::Repository,
    options: Option<&WorktreeDiffOptions>,
) -> napi::Result<Vec<Change>> {
    let (rewrites, patterns, untracked, submodule, _tree_renames) = worktree_settings(options);
    index_worktree_changes(r, rewrites, patterns, untracked, submodule)
}

fn compute_tree_worktree_diff(
    r: &gix::Repository,
    tree: Option<&str>,
    options: Option<&WorktreeDiffOptions>,
) -> napi::Result<Vec<Change>> {
    let (rewrites, patterns, untracked, submodule, tree_renames) = worktree_settings(options);
    let tree_id = resolve_tree_id(r, tree)?;
    // Compose the tree↔index and index↔worktree halves (gix's combined status iterator
    // yields its items through a private enum, so we run both halves explicitly).
    let mut out = tree_index_changes(r, &tree_id, tree_renames, &patterns)?;
    out.extend(index_worktree_changes(
        r, rewrites, patterns, untracked, submodule,
    )?);
    Ok(out)
}

#[napi]
impl Repository {
    /// Diff two trees. When `new_id` is omitted (or `old` is omitted), the corresponding side is
    /// treated as the empty tree, so `diffTreeToTree(tree, null)` reports deletions and
    /// `diffTreeToTree(null, tree)` reports additions.
    ///
    /// `options` controls rename/copy detection; when omitted, repository configuration
    /// decides (rename tracking on at 50% similarity by default).
    #[napi]
    pub fn diff_tree_to_tree(
        &self,
        old: Option<String>,
        new_id: Option<String>,
        options: Option<DiffOptions>,
    ) -> napi::Result<Vec<Change>> {
        let r = thread_local(&self.inner);
        compute_tree_diff(&r, old.as_deref(), new_id.as_deref(), options.as_ref())
    }

    /// Diff two trees without blocking the event loop.
    #[napi(ts_return_type = "Promise<Array<Change>>")]
    pub fn diff_tree_to_tree_async(
        &self,
        old: Option<String>,
        new_id: Option<String>,
        options: Option<DiffOptions>,
    ) -> napi::Result<AsyncTask<TreeDiffTask>> {
        Ok(AsyncTask::new(TreeDiffTask {
            repo: Arc::clone(&self.inner),
            old,
            new_id,
            options,
        }))
    }

    /// Diff the HEAD tree (or the given `tree`) against the index.
    #[napi]
    pub fn diff_tree_to_index(
        &self,
        tree: Option<String>,
        options: Option<DiffOptions>,
    ) -> napi::Result<Vec<Change>> {
        let r = thread_local(&self.inner);
        compute_tree_index_diff(&r, tree.as_deref(), options.as_ref())
    }

    /// Diff a tree against the index without blocking the event loop.
    #[napi(ts_return_type = "Promise<Array<Change>>")]
    pub fn diff_tree_to_index_async(
        &self,
        tree: Option<String>,
        options: Option<DiffOptions>,
    ) -> napi::Result<AsyncTask<TreeIndexDiffTask>> {
        Ok(AsyncTask::new(TreeIndexDiffTask {
            repo: Arc::clone(&self.inner),
            tree,
            options,
        }))
    }

    /// Diff the index against the working tree, including untracked files.
    #[napi]
    pub fn diff_index_to_workdir(
        &self,
        options: Option<WorktreeDiffOptions>,
    ) -> napi::Result<Vec<Change>> {
        let r = thread_local(&self.inner);
        compute_index_worktree_diff(&r, options.as_ref())
    }

    /// Diff the index against the working tree without blocking the event loop.
    #[napi(ts_return_type = "Promise<Array<Change>>")]
    pub fn diff_index_to_workdir_async(
        &self,
        options: Option<WorktreeDiffOptions>,
    ) -> napi::Result<AsyncTask<IndexWorktreeDiffTask>> {
        Ok(AsyncTask::new(IndexWorktreeDiffTask {
            repo: Arc::clone(&self.inner),
            options,
        }))
    }

    /// Diff a tree (default: HEAD) against the working tree, combining the
    /// tree-to-index and index-to-worktree comparisons.
    #[napi]
    pub fn diff_tree_to_workdir(
        &self,
        tree: Option<String>,
        options: Option<WorktreeDiffOptions>,
    ) -> napi::Result<Vec<Change>> {
        let r = thread_local(&self.inner);
        compute_tree_worktree_diff(&r, tree.as_deref(), options.as_ref())
    }

    /// Diff a tree against the working tree without blocking the event loop.
    #[napi(ts_return_type = "Promise<Array<Change>>")]
    pub fn diff_tree_to_workdir_async(
        &self,
        tree: Option<String>,
        options: Option<WorktreeDiffOptions>,
    ) -> napi::Result<AsyncTask<TreeWorktreeDiffTask>> {
        Ok(AsyncTask::new(TreeWorktreeDiffTask {
            repo: Arc::clone(&self.inner),
            tree,
            options,
        }))
    }

    /// Aggregate line/change statistics for a tree-to-tree diff (mirrors `git diff --stat`).
    /// Equivalent to [`diff_tree_to_tree`](Self::diff_tree_to_tree) followed by per-file line counting.
    /// Rewrite tracking is disabled internally for performance; this does not affect the counts.
    #[napi]
    pub fn diff_tree_to_tree_stats(
        &self,
        old: Option<String>,
        new_id: Option<String>,
    ) -> napi::Result<DiffStats> {
        let r = thread_local(&self.inner);
        let old_tree = match old {
            Some(o) => r
                .find_tree(parse_oid(&o)?)
                .map_err(|e| err("find_tree", e))?,
            None => r.empty_tree(),
        };
        let new_tree = match new_id {
            Some(n) => r
                .find_tree(parse_oid(&n)?)
                .map_err(|e| err("find_tree", e))?,
            None => r.empty_tree(),
        };
        let mut platform = old_tree.changes().map_err(|e| err("changes", e))?;
        platform.options(|o| {
            o.track_rewrites(None);
        });
        let stats = platform.stats(&new_tree).map_err(|e| err("stats", e))?;
        Ok(DiffStats {
            files_changed: stats.files_changed as f64,
            lines_added: stats.lines_added as f64,
            lines_removed: stats.lines_removed as f64,
        })
    }

    /// Summarize the working tree and index in one `git status` call: the current branch,
    /// its upstream tracking ref, ahead/behind commit counts, and the staged / unstaged /
    /// untracked path lists.
    ///
    /// Everything is derived from existing gix-backed methods (`head_name`,
    /// `branch_remote_tracking_ref_name`, `rev_walk`, `diff_tree_to_index`,
    /// `diff_index_to_workdir`) — no `git` binary is spawned. For large repositories use
    /// [`changes_async`](Self::changes_async).
    #[napi]
    pub fn changes(&self) -> napi::Result<Changes> {
        // `head_name` returns the full ref (`refs/heads/main`); shorten for display and for
        // the upstream lookup, which expects a short branch name.
        let branch = self
            .head_name()
            .ok()
            .flatten()
            .map(|b| b.strip_prefix("refs/heads/").unwrap_or(&b).to_string());

        let (tracking, ahead, behind) = match &branch {
            Some(b) => {
                let tracking = self
                    .branch_remote_tracking_ref_name(b.clone(), None)
                    .ok()
                    .flatten();
                match &tracking {
                    Some(t) => {
                        let head_id = self.head_id().ok();
                        let up_id = self.rev_parse_single(t.clone()).ok();
                        match (head_id, up_id) {
                            (Some(h), Some(u)) => {
                                let ahead = self
                                    .rev_walk(
                                        vec![h.clone()],
                                        Some(WalkOptions {
                                            sort: None,
                                            reverse: None,
                                            first_parent: None,
                                            exclude: Some(vec![u.clone()]),
                                            glob: None,
                                            range: None,
                                            limit: None,
                                        }),
                                    )
                                    .map(|v| v.len() as u32)
                                    .unwrap_or(0);
                                let behind = self
                                    .rev_walk(
                                        vec![u],
                                        Some(WalkOptions {
                                            sort: None,
                                            reverse: None,
                                            first_parent: None,
                                            exclude: Some(vec![h]),
                                            glob: None,
                                            range: None,
                                            limit: None,
                                        }),
                                    )
                                    .map(|v| v.len() as u32)
                                    .unwrap_or(0);
                                (tracking.clone(), ahead, behind)
                            }
                            _ => (tracking.clone(), 0, 0),
                        }
                    }
                    None => (None, 0, 0),
                }
            }
            None => (None, 0, 0),
        };

        let staged = self
            .diff_tree_to_index(None, None)
            .map(|v| v.into_iter().map(|c| c.path).collect::<Vec<_>>())
            .unwrap_or_default();

        let (unstaged, untracked) = match self.diff_index_to_workdir(None) {
            Ok(changes) => {
                let mut unstaged = Vec::new();
                let mut untracked = Vec::new();
                for c in changes {
                    match c.kind.as_str() {
                        "Added" => untracked.push(c.path),
                        "Ignored" => {}
                        _ => unstaged.push(c.path),
                    }
                }
                (unstaged, untracked)
            }
            Err(_) => (Vec::new(), Vec::new()),
        };

        let is_dirty = !staged.is_empty() || !unstaged.is_empty() || !untracked.is_empty();
        Ok(Changes {
            branch,
            tracking,
            ahead,
            behind,
            staged,
            unstaged,
            untracked,
            is_clean: !is_dirty,
            is_dirty,
        })
    }

    /// Async variant of [`changes`](Self::changes) that does not block the event loop.
    #[napi(ts_return_type = "Promise<Changes>")]
    pub fn changes_async(&self) -> napi::Result<AsyncTask<ChangesTask>> {
        Ok(AsyncTask::new(ChangesTask {
            repo: Arc::clone(&self.inner),
        }))
    }
}

/// Blocking working-tree summary, executed on the libuv thread pool by [`AsyncTask`].
pub struct ChangesTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
}

impl Task for ChangesTask {
    type Output = Changes;
    type JsValue = Changes;

    fn compute(&mut self) -> napi::Result<Changes> {
        let repo = Repository {
            inner: Arc::clone(&self.repo),
        };
        repo.changes().map_err(Into::into)
    }

    fn resolve(&mut self, _env: Env, output: Changes) -> napi::Result<Changes> {
        Ok(output)
    }
}

// ---------------------------------------------------------------------------
// Async task structs (blocking work on the libuv thread pool)
// ---------------------------------------------------------------------------

/// Blocking tree-to-tree diff, executed on the libuv thread pool by [`AsyncTask`].
pub struct TreeDiffTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) old: Option<String>,
    pub(crate) new_id: Option<String>,
    pub(crate) options: Option<DiffOptions>,
}

impl Task for TreeDiffTask {
    type Output = Vec<Change>;
    type JsValue = Vec<Change>;

    fn compute(&mut self) -> napi::Result<Vec<Change>> {
        let r = thread_local(&self.repo);
        compute_tree_diff(
            &r,
            self.old.as_deref(),
            self.new_id.as_deref(),
            self.options.as_ref(),
        )
        .map_err(Into::into)
    }

    fn resolve(&mut self, _env: Env, output: Vec<Change>) -> napi::Result<Vec<Change>> {
        Ok(output)
    }
}

/// Blocking tree-to-index diff, executed on the libuv thread pool by [`AsyncTask`].
pub struct TreeIndexDiffTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) tree: Option<String>,
    pub(crate) options: Option<DiffOptions>,
}

impl Task for TreeIndexDiffTask {
    type Output = Vec<Change>;
    type JsValue = Vec<Change>;

    fn compute(&mut self) -> napi::Result<Vec<Change>> {
        let r = thread_local(&self.repo);
        compute_tree_index_diff(&r, self.tree.as_deref(), self.options.as_ref()).map_err(Into::into)
    }

    fn resolve(&mut self, _env: Env, output: Vec<Change>) -> napi::Result<Vec<Change>> {
        Ok(output)
    }
}

/// Blocking index-to-worktree diff, executed on the libuv thread pool by [`AsyncTask`].
pub struct IndexWorktreeDiffTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) options: Option<WorktreeDiffOptions>,
}

impl Task for IndexWorktreeDiffTask {
    type Output = Vec<Change>;
    type JsValue = Vec<Change>;

    fn compute(&mut self) -> napi::Result<Vec<Change>> {
        let r = thread_local(&self.repo);
        compute_index_worktree_diff(&r, self.options.as_ref()).map_err(Into::into)
    }

    fn resolve(&mut self, _env: Env, output: Vec<Change>) -> napi::Result<Vec<Change>> {
        Ok(output)
    }
}

/// Blocking tree-to-worktree diff, executed on the libuv thread pool by [`AsyncTask`].
pub struct TreeWorktreeDiffTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) tree: Option<String>,
    pub(crate) options: Option<WorktreeDiffOptions>,
}

impl Task for TreeWorktreeDiffTask {
    type Output = Vec<Change>;
    type JsValue = Vec<Change>;

    fn compute(&mut self) -> napi::Result<Vec<Change>> {
        let r = thread_local(&self.repo);
        compute_tree_worktree_diff(&r, self.tree.as_deref(), self.options.as_ref())
            .map_err(Into::into)
    }

    fn resolve(&mut self, _env: Env, output: Vec<Change>) -> napi::Result<Vec<Change>> {
        Ok(output)
    }
}

// ---- imara-diff algorithm bindings --------------------------------------

/// The diff algorithm used to compute an edit script.
#[napi]
pub enum Algorithm {
    /// Histogram (patience variant); best human-readable output. This is the default.
    Histogram,
    /// Myers with early-abort heuristics; near-minimal diffs with fast runtime.
    Myers,
    /// Myers without heuristics; guarantees a minimal edit sequence (can be slow).
    MyersMinimal,
}

impl From<Algorithm> for imara::Algorithm {
    fn from(a: Algorithm) -> Self {
        match a {
            Algorithm::Histogram => imara::Algorithm::Histogram,
            Algorithm::Myers => imara::Algorithm::Myers,
            Algorithm::MyersMinimal => imara::Algorithm::MyersMinimal,
        }
    }
}

/// A contiguous region of change between the `before` and `after` token sequences.
#[napi(object)]
pub struct DiffHunk {
    /// Inclusive start index (token/line) in the `before` sequence.
    pub before_start: u32,
    /// Exclusive end index (token/line) in the `before` sequence.
    pub before_end: u32,
    /// Inclusive start index (token/line) in the `after` sequence.
    pub after_start: u32,
    /// Exclusive end index (token/line) in the `after` sequence.
    pub after_end: u32,
}

/// A single changed line produced by [`diff_lines`].
#[derive(Clone)]
#[napi(object)]
pub struct DiffLine {
    /// `"add"` (line present only in `after`), `"del"` (only in `before`), or
    /// `"context"` (unchanged, included only when context lines are requested).
    pub type_: String,
    /// The line content, without a trailing newline.
    pub content: String,
    /// 1-based line number in `before`, or `null` for added lines.
    pub old_line: Option<u32>,
    /// 1-based line number in `after`, or `null` for deleted lines.
    pub new_line: Option<u32>,
}

/// Diff `before` and `after` text line-by-line using the chosen algorithm (defaults to
/// `Histogram`), returning the list of changed hunks in document order.
///
/// Lines are the default token granularity (newline-terminated, including the separator so a
/// change of separator is detected). Use [`Algorithm::Myers`] when tokens have a very small set
/// (e.g. character diffs).
#[napi]
pub fn diff(
    before: String,
    after: String,
    algorithm: Option<Algorithm>,
) -> napi::Result<Vec<DiffHunk>> {
    let algo = algorithm.map(imara::Algorithm::from).unwrap_or_default();
    let input = imara::InternedInput::new(before.as_str(), after.as_str());
    let mut diff = imara::Diff::compute(algo, &input);
    diff.postprocess_lines(&input);
    let hunks = diff
        .hunks()
        .map(|h| DiffHunk {
            before_start: h.before.start,
            before_end: h.before.end,
            after_start: h.after.start,
            after_end: h.after.end,
        })
        .collect();
    Ok(hunks)
}

/// Diff `before` and `after` text line-by-line using the chosen algorithm (defaults to
/// `Histogram`) and return the **actual changed lines** with their type (`add` / `del`) and,
/// where known, their old/new line numbers.
///
/// Unlike [`diff`](diff) which returns only hunk ranges, this returns line contents, so callers
/// can render a diff without re-slicing the inputs. Context lines (unchanged) are omitted; only
/// added and removed lines are reported.
#[napi]
pub fn diff_lines(
    before: String,
    after: String,
    algorithm: Option<Algorithm>,
) -> napi::Result<Vec<DiffLine>> {
    let algo = algorithm.map(imara::Algorithm::from).unwrap_or_default();
    let input = imara::InternedInput::new(before.as_str(), after.as_str());
    let mut diff = imara::Diff::compute(algo, &input);
    diff.postprocess_lines(&input);
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    let mut out = Vec::new();
    for h in diff.hunks() {
        for i in h.before.start..h.before.end {
            if let Some(line) = before_lines.get(i as usize) {
                out.push(DiffLine {
                    type_: "del".to_string(),
                    content: line.to_string(),
                    old_line: Some(i + 1),
                    new_line: None,
                });
            }
        }
        for j in h.after.start..h.after.end {
            if let Some(line) = after_lines.get(j as usize) {
                out.push(DiffLine {
                    type_: "add".to_string(),
                    content: line.to_string(),
                    new_line: Some(j + 1),
                    old_line: None,
                });
            }
        }
    }
    Ok(out)
}
