use std::sync::Arc;

use crate::error::RepositoryResult as Result;
use napi::Task;
use napi::bindgen_prelude::AsyncTask;
use napi_derive::napi;

use crate::error::{err, thread_local};
use crate::repository::Repository;

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Options for [`Repository::dirwalk()`]. Every field starts disabled.
#[napi(object)]
pub struct DirwalkOptions {
    /// Also emit entries that are ignored by `.gitignore`/`.git/info/exclude`
    /// (default `false`, i.e. ignored entries are omitted).
    pub emit_ignored: Option<bool>,
    /// When `emitIgnored` is set, collapse ignored directories into the directory itself
    /// rather than emitting their individual files (default `false`).
    pub emit_ignored_collapsed: Option<bool>,
    /// Also emit entries that are tracked in the index (default `false`, i.e. tracked entries are omitted).
    pub emit_tracked: Option<bool>,
    /// Emit directories that contain no files at any nesting depth (default `false`).
    pub emit_empty_directories: Option<bool>,
    /// Emit untracked entries collapsed into their directory instead of individually (default `false`).
    pub emit_untracked_collapsed: Option<bool>,
    /// Emit pruned entries whose kind is known (default `false`).
    pub emit_pruned: Option<bool>,
    /// Recurse into nested repositories rather than stopping at them (default `false`).
    pub recurse_repositories: Option<bool>,
    /// Probe untracked directories for a bare `.git` repository, which is costly (default `false`).
    pub classify_untracked_bare_repositories: Option<bool>,
    /// Limit the walk to the repository prefix when `patterns` is empty (default `false`).
    pub empty_patterns_match_prefix: Option<bool>,
    /// Prepare the walk for deletion: precious files are treated like expendable ones and
    /// collapsing is more generous with ignored files (default `false`).
    pub for_deletion: Option<bool>,
    /// Also emit entries inside a directory that is about to be collapsed, so they remain
    /// observable (default `false`).
    pub emit_collapsed: Option<bool>,
    /// Treat symlinks pointing to directories like directories when checking exclusion —
    /// a `libgit2` compatibility flag. Git compatibility wants `false`, the default.
    pub symlinks_to_directories_are_ignored_like_directories: Option<bool>,
}

/// A single entry produced by [`Repository::dirwalk()`].
#[napi(object)]
pub struct DirwalkEntry {
    /// Repository-relative path, with `/` separators.
    pub path: String,
    /// `Tracked`, `Untracked`, `Ignored`/`Ignored:<kind>`, or `Pruned`.
    pub status: String,
    /// The on-disk kind: `File`, `Directory`, `Symlink`, `Repository`, or `Untrackable`.
    /// `null` when the kind was not determined.
    pub disk_kind: Option<String>,
    /// How the pathspecs matched: `Always`, `Excluded`, `Prefix`, `WildcardMatch`, or `Verbatim`.
    pub pathspec_match: Option<String>,
    /// Status of the collapsed directory this entry belongs to, when collapsed emission is enabled.
    pub collapsed_directory_status: Option<String>,
}

/// The result of a full directory walk.
#[napi(object)]
pub struct DirwalkResult {
    /// The emitted entries, in walk order.
    pub entries: Vec<DirwalkEntry>,
    /// The root the traversal actually used; always within the working directory.
    /// Useful to turn `entry.path` back into a filesystem path.
    pub traversal_root: String,
    /// How often a directory's contents had to be read from disk.
    pub read_dir_calls: u32,
    /// The amount of entries provided to the callback; may be lower than `seenEntries`
    /// when entries were collapsed or filtered.
    pub returned_entries: u32,
    /// The amount of entries seen before pathspec filtering and collapsing.
    pub seen_entries: u32,
}

fn status_str(status: &gix::dir::entry::Status) -> String {
    match status {
        gix::dir::entry::Status::Pruned => "Pruned".to_string(),
        gix::dir::entry::Status::Tracked => "Tracked".to_string(),
        gix::dir::entry::Status::Ignored(kind) => format!("Ignored:{kind:?}"),
        gix::dir::entry::Status::Untracked => "Untracked".to_string(),
    }
}

fn kind_str(kind: &gix::dir::entry::Kind) -> String {
    match kind {
        gix::dir::entry::Kind::Untrackable => "Untrackable",
        gix::dir::entry::Kind::File => "File",
        gix::dir::entry::Kind::Symlink => "Symlink",
        gix::dir::entry::Kind::Directory => "Directory",
        gix::dir::entry::Kind::Repository => "Repository",
    }
    .to_string()
}

fn pathspec_match_str(m: gix::dir::entry::PathspecMatch) -> String {
    match m {
        gix::dir::entry::PathspecMatch::Always => "Always",
        gix::dir::entry::PathspecMatch::Excluded => "Excluded",
        gix::dir::entry::PathspecMatch::Prefix => "Prefix",
        gix::dir::entry::PathspecMatch::WildcardMatch => "WildcardMatch",
        gix::dir::entry::PathspecMatch::Verbatim => "Verbatim",
    }
    .to_string()
}

fn to_entry(item: gix::dirwalk::iter::Item) -> DirwalkEntry {
    DirwalkEntry {
        path: lossy(&item.entry.rela_path),
        status: status_str(&item.entry.status),
        disk_kind: item.entry.disk_kind.as_ref().map(kind_str),
        pathspec_match: item.entry.pathspec_match.map(pathspec_match_str),
        collapsed_directory_status: item.collapsed_directory_status.as_ref().map(status_str),
    }
}

/// Blocking directory walk, executed on the libuv thread pool.
pub struct DirwalkTask {
    repo: Arc<gix::ThreadSafeRepository>,
    patterns: Vec<String>,
    options: Option<DirwalkOptions>,
}

impl Task for DirwalkTask {
    type Output = DirwalkResult;
    type JsValue = DirwalkResult;

    fn compute(&mut self) -> napi::Result<DirwalkResult> {
        let r = thread_local(&self.repo);
        let index = r
            .index_or_load_from_head_or_empty()
            .map_err(|e| err("index", e))?;
        let base = r.dirwalk_options().map_err(|e| err("dirwalk_options", e))?;
        let options = match self.options.take() {
            Some(o) => o.into_gix(base),
            None => base,
        };

        // `Default::default()` for the interrupt flag means the walk cannot be interrupted.
        let mut iter = r
            .dirwalk_iter(index, self.patterns.clone(), Default::default(), options)
            .map_err(|e| err("dirwalk", e))?;

        let mut entries = Vec::new();
        for item in &mut iter {
            let item = item.map_err(|e| err("dirwalk_item", e))?;
            entries.push(to_entry(item));
        }

        // Without the `parallel` feature the walk completes during construction, so the
        // outcome is always available once every item has been drained.
        let outcome = iter
            .into_outcome()
            .ok_or_else(|| napi::Error::from_reason("dirwalk ended without an outcome"))?;
        let dirwalk = outcome.dirwalk;
        Ok(DirwalkResult {
            returned_entries: entries.len() as u32,
            entries,
            traversal_root: lossy(outcome.traversal_root.to_string_lossy().as_bytes()),
            read_dir_calls: dirwalk.read_dir_calls,
            seen_entries: dirwalk.seen_entries,
        })
    }

    fn resolve(&mut self, _env: napi::Env, output: DirwalkResult) -> napi::Result<DirwalkResult> {
        Ok(output)
    }
}

impl DirwalkOptions {
    /// Apply the caller's overrides onto options resolved from this repository's
    /// filesystem capabilities.
    fn into_gix(self, base: gix::dirwalk::Options) -> gix::dirwalk::Options {
        let emit_ignored = if self.emit_ignored.unwrap_or(false) {
            Some(if self.emit_ignored_collapsed.unwrap_or(false) {
                gix::dir::walk::EmissionMode::CollapseDirectory
            } else {
                gix::dir::walk::EmissionMode::Matching
            })
        } else {
            None
        };
        base.emit_ignored(emit_ignored)
            .emit_tracked(self.emit_tracked.unwrap_or(false))
            .emit_empty_directories(self.emit_empty_directories.unwrap_or(false))
            .emit_untracked(if self.emit_untracked_collapsed.unwrap_or(false) {
                gix::dir::walk::EmissionMode::CollapseDirectory
            } else {
                gix::dir::walk::EmissionMode::Matching
            })
            .emit_pruned(self.emit_pruned.unwrap_or(false))
            .recurse_repositories(self.recurse_repositories.unwrap_or(false))
            .classify_untracked_bare_repositories(
                self.classify_untracked_bare_repositories.unwrap_or(false),
            )
            .empty_patterns_match_prefix(self.empty_patterns_match_prefix.unwrap_or(false))
            .for_deletion(if self.for_deletion.unwrap_or(false) {
                Some(gix::dir::walk::ForDeletionMode::default())
            } else {
                None
            })
            .emit_collapsed(if self.emit_collapsed.unwrap_or(false) {
                Some(gix::dir::walk::CollapsedEntriesEmissionMode::All)
            } else {
                None
            })
            .symlinks_to_directories_are_ignored_like_directories(
                self.symlinks_to_directories_are_ignored_like_directories
                    .unwrap_or(false),
            )
    }
}

#[napi]
impl Repository {
    /// Walk the working tree, returning entries filtered by `patterns`.
    ///
    /// Requires a working tree (fails on bare repositories). By default only *untracked*
    /// entries are emitted — tracked entries are omitted unless `emitTracked` is set, and
    /// ignored entries are omitted unless `emitIgnored` is set.
    ///
    /// The walk runs on the libuv thread pool since it can be expensive.
    #[napi(ts_return_type = "Promise<DirwalkResult>")]
    pub fn dirwalk(
        &self,
        patterns: Vec<String>,
        options: Option<DirwalkOptions>,
    ) -> napi::Result<AsyncTask<DirwalkTask>> {
        Ok(AsyncTask::new(DirwalkTask {
            repo: Arc::clone(&self.inner),
            patterns,
            options,
        }))
    }
}
