use napi::Result;
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};
use crate::repository::{Repository, lossy};
use gix_imara_diff as imara;

/// A single change produced by [`Repository::diff_tree_to_tree`].
#[derive(Clone)]
#[napi(object)]
pub struct Change {
    /// Repo-relative path of the change. For renames/copies this is the *destination* path.
    pub path: String,
    /// Source path for renames/copies; `None` for additions, modifications, and deletions.
    pub source_location: Option<String>,
    pub old_id: Option<String>,
    pub new_id: Option<String>,
    /// The kind of change: `Added`, `Modified`, `Deleted`, `Renamed`, `Copied`.
    pub kind: String,
    /// Git mode of the entry after the change, as an octal string (`100644`, `100755`, `040000`, `120000`).
    pub entry_mode: String,
    /// Git mode of the entry before the change (modifications and renames/copies only); `None` otherwise.
    pub previous_entry_mode: Option<String>,
}

fn mode_string(mode: gix::object::tree::EntryMode) -> String {
    format!("{:06o}", mode.value())
}

/// Aggregated line/change statistics for a tree-to-tree diff (mirrors `git diff --stat`).
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

#[napi]
impl Repository {
    /// Diff two trees. When `new_id` is omitted (or `old` is omitted), the corresponding side is
    /// treated as the empty tree, so `diff_tree_to_tree(tree, null)` reports deletions and
    /// `diff_tree_to_tree(null, tree)` reports additions.
    ///
    /// `track_rewrites` controls rename/copy detection: `"renames"` enables rename tracking (git's
    /// default), `"renames-and-copies"` also tracks copies, and `"none"` disables it. When omitted,
    /// repository configuration decides (rename tracking on at 50% similarity by default).
    #[napi]
    pub fn diff_tree_to_tree(
        &self,
        old: Option<String>,
        new_id: Option<String>,
        track_rewrites: Option<String>,
    ) -> Result<Vec<Change>> {
        let r = thread_local(&self.inner);
        let empty = gix::ObjectId::empty_tree(r.object_hash());
        let old_tree = match old {
            Some(o) => Some(
                r.find_tree(parse_oid(&o)?)
                    .map_err(|e| err("find_tree", e))?,
            ),
            None => None,
        };
        let new_tree = match new_id {
            Some(n) => r.find_tree(parse_oid(&n)?).map_err(|e| err("find_tree", e))?,
            None => r.find_tree(empty).map_err(|e| err("find_tree", e))?,
        };
        let options = track_rewrites.as_deref().map(|mode| {
            let mut opts = gix::diff::Options::default();
            match mode {
                "renames" => opts.track_rewrites(Some(gix::diff::Rewrites::default())),
                "renames-and-copies" => opts.track_rewrites(Some(gix::diff::Rewrites {
                    copies: Some(gix::diff::rewrites::Copies::default()),
                    ..Default::default()
                })),
                _ => opts.track_rewrites(None),
            };
            opts
        });
        let changes = r
            .diff_tree_to_tree(old_tree.as_ref(), Some(&new_tree), options)
            .map_err(|e| err("diff_tree_to_tree", e))?;
        Ok(changes
            .into_iter()
            .map(|c| match c {
                gix::object::tree::diff::ChangeDetached::Addition { location, id, entry_mode, .. } => Change {
                    path: lossy(&*location),
                    source_location: None,
                    old_id: None,
                    new_id: Some(id.to_string()),
                    kind: "Added".to_string(),
                    entry_mode: mode_string(entry_mode),
                    previous_entry_mode: None,
                },
                gix::object::tree::diff::ChangeDetached::Deletion { location, id, entry_mode, .. } => Change {
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
                    kind: if copy { "Copied".to_string() } else { "Renamed".to_string() },
                    entry_mode: mode_string(entry_mode),
                    previous_entry_mode: Some(mode_string(source_entry_mode)),
                },
            })
            .collect())
    }

    /// Aggregate line/change statistics for a tree-to-tree diff (mirrors `git diff --stat`).
    /// Equivalent to [`diff_tree_to_tree`](Self::diff_tree_to_tree) followed by per-file line counting.
    /// Rewrite tracking is disabled internally for performance; this does not affect the counts.
    #[napi]
    pub fn diff_tree_to_tree_stats(
        &self,
        old: Option<String>,
        new_id: Option<String>,
    ) -> Result<DiffStats> {
        let r = thread_local(&self.inner);
        let old_tree = match old {
            Some(o) => r.find_tree(parse_oid(&o)?).map_err(|e| err("find_tree", e))?,
            None => r.empty_tree(),
        };
        let new_tree = match new_id {
            Some(n) => r.find_tree(parse_oid(&n)?).map_err(|e| err("find_tree", e))?,
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

/// Diff `before` and `after` text line-by-line using the chosen algorithm (defaults to
/// `Histogram`), returning the list of changed hunks in document order.
///
/// Lines are the default token granularity (newline-terminated, including the separator so a
/// change of separator is detected). Use [`Algorithm::Myers`] when tokens have a very small set
/// (e.g. character diffs).
#[napi]
pub fn diff(before: String, after: String, algorithm: Option<Algorithm>) -> Result<Vec<DiffHunk>> {
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
