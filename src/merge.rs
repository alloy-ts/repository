use crate::error::RepositoryResult;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;
use std::collections::HashMap;

use gix::bstr::{BStr, ByteSlice};
use gix::diff::tree_with_rewrites::Change;
use gix::merge::blob::builtin_driver::text;
use gix::merge::tree::{Resolution, ResolutionFailure, TreatAsUnresolved, apply_index_entries};

use gix_imara_diff as imara_diff;

use crate::error::{err, err_str, parse_oid, thread_local};
use crate::repository::Repository;

/// Resolved options used for blob merges (the `text` driver configuration).
#[napi(object)]
pub struct BlobMergeOptionsSummary {
    /// Whether the resources being merged belong to a virtual ancestor.
    pub is_virtual_ancestor: bool,
    /// The diff algorithm used for text merges (e.g. `Myers`, `Histogram`).
    pub diff_algorithm: String,
    /// How conflicts are handled (e.g. `Keep { style: …, marker_size: 7 }`, `ResolveWithOurs`).
    pub conflict: String,
}

/// Resolved options used for three-way tree merges.
#[napi(object)]
pub struct TreeMergeOptionsSummary {
    /// Whether rename tracking is performed when determining each side's changes.
    pub rewrites_enabled: bool,
    /// How blob-merges are performed (debug summary of the text-merge configuration).
    pub blob_merge: String,
    /// Whether the first unresolved conflict aborts the whole merge.
    pub fail_on_conflict: bool,
    /// Multiplier applied to conflict marker sizes for nested (recursive) merges.
    pub marker_size_multiplier: u32,
    /// How irreconcilable tree conflicts are resolved (debug summary), or `null`
    /// when such conflicts cause resolution failures.
    pub tree_conflicts: Option<String>,
}

/// The result of a (recursive) virtual merge base computation.
#[napi(object)]
pub struct VirtualMergeBaseOutcome {
    /// The id of the commit created to hold the merged tree.
    pub commit_id: String,
    /// The hash of the merged tree.
    pub tree_id: String,
    /// All virtual merge bases produced, oldest first; the last equals `commitId`.
    pub virtual_merge_bases: Vec<String>,
}

#[napi(object)]
pub struct MergeOptions {
    /// Label for the ancestor side of conflict markers.
    pub ancestor_label: Option<String>,
    /// Label for the "our" side of conflict markers.
    pub our_label: Option<String>,
    /// Label for the "their" side of conflict markers.
    pub their_label: Option<String>,
    /// `"ours"` or `"theirs"` — auto-resolve file content, binary and symlink
    /// conflicts by choosing a side (like `-X ours` / `-X theirs`).
    pub file_favor: Option<String>,
    /// `"ours"` or `"ancestor"` — auto-resolve irreconcilable tree conflicts
    /// by choosing a side (or the shared ancestor state).
    pub tree_favor: Option<String>,
    /// Enable rename/copy tracking (`gix_diff::Rewrites::default()`).
    pub rewrites: Option<bool>,
    /// Similarity (0-100) needed to consider a file renamed; like `git diff -M`.
    /// Only applied when `rewrites` is enabled.
    pub rename_threshold: Option<f64>,
    /// Maximum number of files considered for fuzzy rename/copy tracking
    /// (like `git`'s `diff.renameLimit`); `0` means unlimited.
    /// Only applied when `rewrites` is enabled.
    pub rename_limit: Option<u32>,
    /// Also detect copies, not just renames. Only applied when `rewrites` is enabled.
    pub find_copies: Option<bool>,
    /// `"git"`, `"forcedResolution"` or `"undecidable"` — abort the merge at
    /// the first conflict considered unresolved under that criterion.
    pub fail_on_conflict: Option<String>,
    /// Multiplier applied to conflict marker sizes for nested merges.
    pub marker_size_multiplier: Option<u32>,
    /// Commit merges only: use the first merge base instead of a virtual one.
    pub use_first_merge_base: Option<bool>,
    /// Commit merges only: allow merging commits without a merge base.
    pub allow_missing_merge_base: Option<bool>,
}

/// One side of a conflict: an addition, deletion, modification or rewrite.
#[derive(Clone)]
#[napi(object)]
pub struct MergeChange {
    /// `"addition"`, `"deletion"`, `"modification"` or `"rewrite"`.
    pub kind: String,
    /// Repository-relative path of the change (rename destination for rewrites).
    pub location: String,
    /// Tree entry mode in octal (e.g. `"100644"`).
    pub mode: String,
    /// Object id of the entry.
    pub id: String,
    /// Previous path, when it differs from `location` (renames/copies).
    pub source_location: Option<String>,
    /// Previous mode, for modifications and rewrites.
    pub source_mode: Option<String>,
    /// Previous object id, for modifications and rewrites.
    pub source_id: Option<String>,
}

/// A single index entry (stage 0/1/2/3) of a conflicted path.
#[derive(Clone)]
#[napi(object)]
pub struct MergeConflictEntry {
    /// Tree entry mode in octal.
    pub mode: String,
    /// Object id of the entry.
    pub id: String,
}

/// A contiguous conflicting region extracted from a merged blob's conflict
/// markers, for building a custom conflict UI. `baseLines` is populated only
/// when the conflict markers include a base section (the `diff3` / `zealousDiff3`
/// styles); otherwise it is empty.
#[derive(Clone)]
#[napi(object)]
pub struct ConflictHunk {
    pub base_lines: Vec<String>,
    pub left_lines: Vec<String>,
    pub right_lines: Vec<String>,
}

/// A conflict encountered during a tree merge, including auto-resolved ones.
#[napi(object)]
pub struct MergeConflict {
    /// `true` if this conflict would need resolution before the merged tree is
    /// usable (under the standard `git` criteria).
    pub unresolved: bool,
    /// `"resolved:<kind>"` or `"failed:<kind>"`, e.g.
    /// `"resolved:oursModifiedTheirsModifiedThenBlobContentMerge"` or
    /// `"failed:oursModifiedTheirsDeleted"`. `resolved:forced:<kind>` marks
    /// resolution failures that were turned into a resolution by `treeFavor`.
    pub resolution: String,
    /// Final path the change ended up at, when it was moved by resolution.
    pub final_location: Option<String>,
    /// Merged entry mode in octal, when one side added the executable bit.
    pub merged_mode: Option<String>,
    /// Id of the merged blob, when a content merge was performed.
    pub merged_blob_id: Option<String>,
    /// `"complete"`, `"completeWithAutoResolvedConflict"` or `"conflict"`.
    pub merged_blob_resolution: Option<String>,
    /// Non-conflicting path where a clashing entry was moved to.
    pub renamed_unique_path: Option<String>,
    /// Our change, ordered to match `resolution` (may be swapped with theirs).
    pub ours: MergeChange,
    /// Their change, ordered to match `resolution` (may be swapped with ours).
    pub theirs: MergeChange,
    /// Stage 1 (ancestor) index entry, if present.
    pub base_entry: Option<MergeConflictEntry>,
    /// Stage 2 (ours) index entry, if present.
    pub our_entry: Option<MergeConflictEntry>,
    /// Stage 3 (theirs) index entry, if present.
    pub their_entry: Option<MergeConflictEntry>,
    /// Raw blob content of the ancestor (stage 1) side, when this conflict is
    /// unresolved and the ancestor entry is present. `null` otherwise.
    pub base_content: Option<Buffer>,
    /// Raw blob content of the "our" (stage 2) side, when unresolved and present.
    pub left_content: Option<Buffer>,
    /// Raw blob content of the "their" (stage 3) side, when unresolved and present.
    pub right_content: Option<Buffer>,
    /// Conflict regions parsed from the merged blob's markers, for building a
    /// custom conflict UI. Empty when there is no content-merge marker output.
    pub hunks: Vec<ConflictHunk>,
}

impl Clone for MergeConflict {
    fn clone(&self) -> Self {
        Self {
            unresolved: self.unresolved,
            resolution: self.resolution.clone(),
            final_location: self.final_location.clone(),
            merged_mode: self.merged_mode.clone(),
            merged_blob_id: self.merged_blob_id.clone(),
            merged_blob_resolution: self.merged_blob_resolution.clone(),
            renamed_unique_path: self.renamed_unique_path.clone(),
            ours: self.ours.clone(),
            theirs: self.theirs.clone(),
            base_entry: self.base_entry.clone(),
            our_entry: self.our_entry.clone(),
            their_entry: self.their_entry.clone(),
            base_content: self.base_content.as_ref().map(|b| Buffer::from(b.as_ref().to_vec())),
            left_content: self.left_content.as_ref().map(|b| Buffer::from(b.as_ref().to_vec())),
            right_content: self.right_content.as_ref().map(|b| Buffer::from(b.as_ref().to_vec())),
            hunks: self.hunks.clone(),
        }
    }
}

/// The result of a merge operation.
#[derive(Clone)]
#[napi(object)]
pub struct MergeOutcome {
    pub tree_id: String,
    /// `true` if any conflict was encountered — including ones that were
    /// resolved automatically. See `hasUnresolvedConflicts` for the actionable
    /// question and `conflictDetails` for per-path information.
    pub conflicts: bool,
    /// `true` if any conflict still needs manual resolution (git criteria).
    pub has_unresolved_conflicts: bool,
    /// `true` if `failOnConflict` aborted the merge early; the tree is then
    /// only a partial merge that cannot be completed.
    pub failed_on_first_unresolved_conflict: bool,
    /// Details for every conflict, in the order they were encountered.
    pub conflict_details: Vec<MergeConflict>,
    /// Aggregate change + conflict counts for the merge, relative to the merge
    /// base (added/removed/modified/unchanged) plus the unresolved-conflict count.
    pub stats: MergeStats,
}

/// Aggregate counts summarizing a tree merge result, relative to the merge base.
#[derive(Clone)]
#[napi(object)]
pub struct MergeStats {
    /// Files present in the merged tree but not in the base tree.
    pub added: f64,
    /// Files present in the base tree but not in the merged tree.
    pub removed: f64,
    /// Files present in both trees with a different object id.
    pub modified: f64,
    /// Files present in both trees with the same object id.
    pub unchanged: f64,
    /// Conflicts that still need manual resolution (git criteria).
    pub conflicted: f64,
}

pub(crate) struct OwnedLabels {
    ancestor: String,
    current: String,
    other: String,
}

pub(crate) fn owned_labels(options: Option<&MergeOptions>) -> OwnedLabels {
    let default = |o: Option<&MergeOptions>| match o {
        Some(o) => (
            o.ancestor_label
                .clone()
                .unwrap_or_else(|| "ancestor".into()),
            o.our_label.clone().unwrap_or_else(|| "our".into()),
            o.their_label.clone().unwrap_or_else(|| "their".into()),
        ),
        None => ("ancestor".into(), "our".into(), "their".into()),
    };
    let (ancestor, current, other) = default(options);
    OwnedLabels {
        ancestor,
        current,
        other,
    }
}

impl OwnedLabels {
    pub(crate) fn borrowed(&self) -> text::Labels<'_> {
        text::Labels {
            ancestor: Some(self.ancestor.as_bytes().into()),
            current: Some(self.current.as_bytes().into()),
            other: Some(self.other.as_bytes().into()),
        }
    }
}

fn parse_treat_as_unresolved(what: &str, s: &str) -> napi::Result<TreatAsUnresolved> {
    match s {
        "git" => Ok(TreatAsUnresolved::git()),
        "forcedResolution" => Ok(TreatAsUnresolved::forced_resolution()),
        "undecidable" => Ok(TreatAsUnresolved::undecidable()),
        other => Err(err_str(
            what,
            format!(
                "invalid unresolved preset {other:?} (expected \"git\", \"forcedResolution\" or \"undecidable\")"
            ),
        )),
    }
}

/// Build facade-level tree merge options from the JS-facing [`MergeOptions`].
///
/// Uses the plumbing-level options for `marker_size_multiplier`, which the
/// facade wrapper cannot express (its `inner` field is private).
pub(crate) fn build_tree_options(
    what: &str,
    options: Option<&MergeOptions>,
) -> napi::Result<gix::merge::tree::Options> {
    let mut facade = gix::merge::tree::Options::default();
    if let Some(o) = options {
        if let Some(rewrites) = o.rewrites {
            let mut rw = gix::diff::Rewrites::default();
            if let Some(threshold) = o.rename_threshold {
                rw.percentage = Some((threshold / 100.0).clamp(0.0, 1.0) as f32);
            }
            if let Some(limit) = o.rename_limit {
                rw.limit = limit as usize;
            }
            if let Some(find_copies) = o.find_copies {
                rw.copies = find_copies.then(gix::diff::rewrites::Copies::default);
            }
            facade = facade.with_rewrites(rewrites.then_some(rw));
        }
        if let Some(fail_on_conflict) = o.fail_on_conflict.as_deref() {
            let how = parse_treat_as_unresolved(what, fail_on_conflict)?;
            facade = facade.with_fail_on_conflict(Some(how));
        }
        if let Some(file_favor) = o.file_favor.as_deref() {
            let favor = match file_favor {
                "ours" => gix::merge::tree::FileFavor::Ours,
                "theirs" => gix::merge::tree::FileFavor::Theirs,
                other => {
                    return Err(err_str(
                        what,
                        format!("invalid file_favor {other:?} (expected \"ours\" or \"theirs\")"),
                    ));
                }
            };
            facade = facade.with_file_favor(Some(favor));
        }
        if let Some(tree_favor) = o.tree_favor.as_deref() {
            let favor = match tree_favor {
                "ours" => gix::merge::tree::TreeFavor::Ours,
                "ancestor" => gix::merge::tree::TreeFavor::Ancestor,
                other => {
                    return Err(err_str(
                        what,
                        format!("invalid tree_favor {other:?} (expected \"ours\" or \"ancestor\")"),
                    ));
                }
            };
            facade = facade.with_tree_favor(Some(favor));
        }
    }
    let mut plumbing: gix::merge::plumbing::tree::Options = facade.into();
    if let Some(multiplier) = options.and_then(|o| o.marker_size_multiplier) {
        plumbing.marker_size_multiplier = u8::try_from(multiplier).unwrap_or(u8::MAX);
    }
    Ok(plumbing.into())
}

/// Build commit-level merge options, including commit-specific switches.
fn build_commit_options(
    what: &str,
    options: Option<&MergeOptions>,
) -> napi::Result<gix::merge::commit::Options> {
    let mut commit: gix::merge::commit::Options = build_tree_options(what, options)?.into();
    if let Some(o) = options {
        if let Some(use_first) = o.use_first_merge_base {
            commit = commit.with_use_first_merge_base(use_first);
        }
        if let Some(allow_missing) = o.allow_missing_merge_base {
            commit = commit.with_allow_missing_merge_base(allow_missing);
        }
    }
    Ok(commit)
}

fn mode_string(mode: gix::object::tree::EntryMode) -> String {
    format!("{:06o}", mode.value())
}

fn change_kind(change: &Change) -> &'static str {
    match change {
        Change::Addition { .. } => "addition",
        Change::Deletion { .. } => "deletion",
        Change::Modification { .. } => "modification",
        Change::Rewrite { .. } => "rewrite",
    }
}

fn change_to_dto(change: &Change) -> MergeChange {
    let (mode, id) = change.entry_mode_and_id();
    let (source_mode, source_id) = change.source_entry_mode_and_id();
    let location = change.location();
    let source_location = change.source_location();
    // Additions have no meaningful source (it equals the current entry).
    let has_source = !matches!(change, Change::Addition { .. });
    MergeChange {
        kind: change_kind(change).into(),
        location: location.to_string(),
        mode: mode_string(mode),
        id: id.to_string(),
        source_location: (has_source && source_location != location)
            .then(|| source_location.to_string()),
        source_mode: has_source.then(|| mode_string(source_mode)),
        source_id: has_source.then(|| source_id.to_string()),
    }
}

fn blob_resolution_string(resolution: &gix::merge::blob::Resolution) -> &'static str {
    match resolution {
        gix::merge::blob::Resolution::Complete => "complete",
        gix::merge::blob::Resolution::CompleteWithAutoResolvedConflict => {
            "completeWithAutoResolvedConflict"
        }
        gix::merge::blob::Resolution::Conflict => "conflict",
    }
}

fn failure_kind(failure: &ResolutionFailure) -> &'static str {
    match failure {
        ResolutionFailure::SubmoduleMerge => "submoduleMerge",
        ResolutionFailure::SubmoduleAddAdd => "submoduleAddAdd",
        ResolutionFailure::OursRenamedTheirsRenamedToSameLocation => {
            "oursRenamedTheirsRenamedToSameLocation"
        }
        ResolutionFailure::OursRenamedTheirsRenamedDifferently { .. } => {
            "oursRenamedTheirsRenamedDifferently"
        }
        ResolutionFailure::OursModifiedTheirsDirectoryThenOursRenamed { .. } => {
            "oursModifiedTheirsDirectoryThenOursRenamed"
        }
        ResolutionFailure::OursDirectoryTheirsNonDirectoryTheirsRenamed { .. } => {
            "oursDirectoryTheirsNonDirectoryTheirsRenamed"
        }
        ResolutionFailure::OursAddedTheirsAddedTypeMismatch { .. } => {
            "oursAddedTheirsAddedTypeMismatch"
        }
        ResolutionFailure::OursModifiedTheirsRenamedTypeMismatch => {
            "oursModifiedTheirsRenamedTypeMismatch"
        }
        ResolutionFailure::OursDeletedTheirsRenamed => "oursDeletedTheirsRenamed",
        ResolutionFailure::OursModifiedTheirsDeleted => "oursModifiedTheirsDeleted",
        ResolutionFailure::Unknown => "unknown",
    }
}

fn conflict_to_dto(
    r: &gix::Repository,
    conflict: &gix::merge::tree::Conflict,
    how: TreatAsUnresolved,
) -> MergeConflict {
    let (ours, theirs) = conflict.changes_in_resolution();
    let [base, our, their] = conflict.entries();
    let entry_dto = |entry: &Option<gix::merge::plumbing::tree::ConflictIndexEntry>| {
        entry.as_ref().map(|e| MergeConflictEntry {
            mode: mode_string(e.mode),
            id: e.id.to_string(),
        })
    };
    // Raw triple content for unresolved conflicts (stage 1/2/3 blobs).
    let read_blob =
        |entry: &Option<gix::merge::plumbing::tree::ConflictIndexEntry>| -> Option<Buffer> {
            entry.as_ref().and_then(|e| {
                r.find_object(e.id)
                    .ok()
                    .map(|o| Buffer::from(o.detach().data))
            })
        };
    let base_content = read_blob(&base);
    let left_content = read_blob(&our);
    let right_content = read_blob(&their);

    let resolution;
    let mut final_location = None;
    let mut merged_mode = None;
    let mut renamed_unique_path = None;
    match &conflict.resolution {
        Ok(Resolution::SourceLocationAffectedByRename {
            final_location: loc,
        }) => {
            resolution = "resolved:sourceLocationAffectedByRename".into();
            final_location = Some(loc.to_string());
        }
        Ok(Resolution::OursModifiedTheirsRenamedAndChangedThenRename {
            merged_mode: mode,
            final_location: loc,
            ..
        }) => {
            resolution = "resolved:oursModifiedTheirsRenamedAndChangedThenRename".into();
            merged_mode = mode.map(mode_string);
            final_location = loc.as_ref().map(|l| l.to_string());
        }
        Ok(Resolution::OursModifiedTheirsModifiedThenBlobContentMerge { .. }) => {
            resolution = "resolved:oursModifiedTheirsModifiedThenBlobContentMerge".into();
        }
        Ok(Resolution::Forced(failure)) => {
            resolution = format!("resolved:forced:{}", failure_kind(failure));
        }
        Err(failure) => {
            resolution = format!("failed:{}", failure_kind(failure));
            renamed_unique_path = match failure {
                ResolutionFailure::OursModifiedTheirsDirectoryThenOursRenamed {
                    renamed_unique_path_to_modified_blob,
                }
                | ResolutionFailure::OursDirectoryTheirsNonDirectoryTheirsRenamed {
                    renamed_unique_path_of_theirs: renamed_unique_path_to_modified_blob,
                }
                | ResolutionFailure::OursAddedTheirsAddedTypeMismatch {
                    their_unique_location: renamed_unique_path_to_modified_blob,
                } => Some(renamed_unique_path_to_modified_blob.to_string()),
                _ => None,
            };
        }
    }
    let merged_blob = conflict.content_merge();
    let (merged_blob_id, merged_blob_resolution, hunks) = match merged_blob {
        Some(merged) => {
            let id = merged.merged_blob_id;
            (
                Some(id.to_string()),
                Some(blob_resolution_string(&merged.resolution).to_string()),
                build_hunks(r, &id),
            )
        }
        None => (None, None, Vec::new()),
    };

    MergeConflict {
        unresolved: conflict.is_unresolved(how),
        resolution,
        final_location,
        merged_mode,
        merged_blob_id,
        merged_blob_resolution,
        renamed_unique_path,
        ours: change_to_dto(ours),
        theirs: change_to_dto(theirs),
        base_entry: entry_dto(&base),
        our_entry: entry_dto(&our),
        their_entry: entry_dto(&their),
        base_content,
        left_content,
        right_content,
        hunks,
    }
}

/// Convert a merge outcome into the JS-facing [`MergeOutcome`], writing the
/// unwritten editor tree into the object database first.
pub(crate) fn merge_outcome_to_dto(
    r: &gix::Repository,
    outcome: &mut gix::merge::tree::Outcome<'_>,
    base_tree: Option<&gix::ObjectId>,
    what: &str,
) -> napi::Result<MergeOutcome> {
    // The standard (git) criteria for what still needs manual resolution.
    let how = TreatAsUnresolved::git();
    let conflict_details = outcome
        .conflicts
        .iter()
        .map(|c| conflict_to_dto(r, c, how))
        .collect();
    let tree_obj_id = outcome.tree.write().map_err(|e| err(what, e))?.detach();
    let conflicted = outcome
        .conflicts
        .iter()
        .filter(|c| c.is_unresolved(how))
        .count() as f64;
    let stats = match base_tree {
        Some(base) => compute_merge_stats(r, base, &tree_obj_id, conflicted)?,
        None => MergeStats {
            added: 0.0,
            removed: 0.0,
            modified: 0.0,
            unchanged: 0.0,
            conflicted,
        },
    };
    Ok(MergeOutcome {
        tree_id: tree_obj_id.to_string(),
        conflicts: !outcome.conflicts.is_empty(),
        has_unresolved_conflicts: outcome.has_unresolved_conflicts(how),
        failed_on_first_unresolved_conflict: outcome.failed_on_first_unresolved_conflict,
        conflict_details,
        stats,
    })
}

/// Flatten `tree_id` into `path -> object id` (recursing into subtrees).
fn collect_tree_entries(
    r: &gix::Repository,
    tree_id: &gix::ObjectId,
) -> napi::Result<HashMap<String, gix::ObjectId>> {
    let mut map = HashMap::new();
    fn recurse(
        r: &gix::Repository,
        map: &mut HashMap<String, gix::ObjectId>,
        prefix: &str,
        tree: &gix::Tree,
    ) -> napi::Result<()> {
        for entry in tree.iter() {
            let entry = entry.map_err(|e| err("tree_entry", e))?;
            let name = entry.filename().to_str_lossy().into_owned();
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{}/{}", prefix, name)
            };
            if entry.mode().is_tree() {
                let sub = r.find_tree(entry.id()).map_err(|e| err("find_tree", e))?;
                recurse(r, map, &path, &sub)?;
            } else {
                map.insert(path, entry.id().detach());
            }
        }
        Ok(())
    }
    let tree = r.find_tree(*tree_id).map_err(|e| err("find_tree", e))?;
    recurse(r, &mut map, "", &tree)?;
    Ok(map)
}

/// Aggregate base-relative change counts for a merge result, plus the unresolved
/// conflict count.
fn compute_merge_stats(
    r: &gix::Repository,
    base_tree: &gix::ObjectId,
    merged_tree: &gix::ObjectId,
    conflicted: f64,
) -> napi::Result<MergeStats> {
    let base = collect_tree_entries(r, base_tree)?;
    let merged = collect_tree_entries(r, merged_tree)?;
    let mut added = 0u64;
    let mut removed = 0u64;
    let mut modified = 0u64;
    let mut unchanged = 0u64;
    for (path, mid) in &merged {
        match base.get(path) {
            Some(bid) if bid == mid => unchanged += 1,
            Some(_) => modified += 1,
            None => added += 1,
        }
    }
    for path in base.keys() {
        if !merged.contains_key(path) {
            removed += 1;
        }
    }
    Ok(MergeStats {
        added: added as f64,
        removed: removed as f64,
        modified: modified as f64,
        unchanged: unchanged as f64,
        conflicted,
    })
}

/// Read the merged blob and parse its conflict markers into per-hunk line arrays
/// suitable for a custom conflict UI. Returns an empty list when there is no
/// marker output.
fn build_hunks(r: &gix::Repository, blob_id: &gix::ObjectId) -> Vec<ConflictHunk> {
    match r.find_object(*blob_id) {
        Ok(o) => parse_conflict_hunks(&String::from_utf8_lossy(&o.detach().data)),
        Err(_) => Vec::new(),
    }
}

/// Scan `text` for nested `<<<<<<<` / `>>>>>>>` conflict blocks (honouring the
/// `=======` and optional `|||||||` base separators) and return one [`ConflictHunk`]
/// per block with the base/our/their line sets.
fn parse_conflict_hunks(text: &str) -> Vec<ConflictHunk> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut hunks = Vec::new();
    let mut i = 0;
    #[derive(PartialEq)]
    enum Section {
        Base,
        Ours,
        Theirs,
    }
    while i < lines.len() {
        let line = lines[i];
        if line.starts_with("<<<<<<<") {
            let mut base: Vec<String> = Vec::new();
            let mut ours: Vec<String> = Vec::new();
            let mut theirs: Vec<String> = Vec::new();
            let mut section = Section::Ours;
            let mut depth = 1;
            i += 1;
            while i < lines.len() {
                let l = lines[i];
                if l.starts_with("<<<<<<<") {
                    depth += 1;
                    section = Section::Ours;
                    ours.push(l.to_string());
                } else if l.starts_with(">>>>>>>") {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                    theirs.push(l.to_string());
                } else if l.starts_with("|||||||") {
                    section = Section::Base;
                } else if l.starts_with("=======") {
                    section = Section::Theirs;
                } else {
                    match section {
                        Section::Base => base.push(l.to_string()),
                        Section::Ours => ours.push(l.to_string()),
                        Section::Theirs => theirs.push(l.to_string()),
                    }
                }
                i += 1;
            }
            hunks.push(ConflictHunk {
                base_lines: base,
                left_lines: ours,
                right_lines: theirs,
            });
        } else {
            i += 1;
        }
    }
    hunks
}

#[napi]
impl Repository {
    /// The merge base of two commits, if any.
    #[napi]
    pub fn merge_base(&self, one: String, two: String) -> napi::Result<Option<String>> {
        let r = thread_local(&self.inner);
        let base = r
            .merge_base(parse_oid(&one)?, parse_oid(&two)?)
            .map_err(|e| err("merge_base", e))?;
        Ok(base.map(|id| id.to_string()))
    }

    /// The (octopus) merge base of multiple commits, if any.
    #[napi]
    pub fn merge_base_octopus(&self, commits: Vec<String>) -> napi::Result<Option<String>> {
        let r = thread_local(&self.inner);
        let ids: Vec<gix::ObjectId> = commits
            .iter()
            .map(|c| parse_oid(c))
            .collect::<napi::Result<Vec<_>>>()?;
        let base = r
            .merge_base_octopus(ids)
            .map_err(|e| err("merge_base_octopus", e))?;
        Ok(base.map(|id| id.to_string()))
    }

    /// The merge base of two commits, building the revision graph on demand (uses the commit-graph if enabled).
    #[napi]
    pub fn merge_base_with_graph(&self, one: String, two: String) -> napi::Result<Option<String>> {
        let r = thread_local(&self.inner);
        let cache = r
            .commit_graph_if_enabled()
            .map_err(|e| err("commit_graph_if_enabled", e))?;
        let mut graph = r.revision_graph(cache.as_ref());
        let base = r
            .merge_base_with_graph(parse_oid(&one)?, parse_oid(&two)?, &mut graph)
            .map_err(|e| err("merge_base_with_graph", e))?;
        Ok(base.map(|id| id.to_string()))
    }

    /// All merge bases between `one` and the `others` commits.
    #[napi]
    pub fn merge_bases_many(&self, one: String, others: Vec<String>) -> napi::Result<Vec<String>> {
        let r = thread_local(&self.inner);
        let one_id = parse_oid(&one)?;
        let others_ids: Vec<gix::ObjectId> = others
            .iter()
            .map(|o| parse_oid(o))
            .collect::<napi::Result<Vec<_>>>()?;
        let bases = r
            .merge_bases_many(one_id, &others_ids)
            .map_err(|e| err("merge_bases_many", e))?;
        Ok(bases.into_iter().map(|id| id.to_string()).collect())
    }

    /// All merge bases between `one` and the `others` commits, using an explicit revision graph.
    #[napi]
    pub fn merge_bases_many_with_graph(
        &self,
        one: String,
        others: Vec<String>,
    ) -> napi::Result<Vec<String>> {
        let r = thread_local(&self.inner);
        let one_id = parse_oid(&one)?;
        let others_ids: Vec<gix::ObjectId> = others
            .iter()
            .map(|o| parse_oid(o))
            .collect::<napi::Result<Vec<_>>>()?;
        let cache = r
            .commit_graph_if_enabled()
            .map_err(|e| err("commit_graph_if_enabled", e))?;
        let mut graph = r.revision_graph(cache.as_ref());
        let bases = r
            .merge_bases_many_with_graph(one_id, &others_ids, &mut graph)
            .map_err(|e| err("merge_bases_many_with_graph", e))?;
        Ok(bases.into_iter().map(|id| id.to_string()).collect())
    }

    /// The (octopus) merge base of multiple commits, using an explicit revision graph.
    #[napi]
    pub fn merge_base_octopus_with_graph(
        &self,
        commits: Vec<String>,
    ) -> napi::Result<Option<String>> {
        let r = thread_local(&self.inner);
        let ids: Vec<gix::ObjectId> = commits
            .iter()
            .map(|c| parse_oid(c))
            .collect::<napi::Result<Vec<_>>>()?;
        let cache = r
            .commit_graph_if_enabled()
            .map_err(|e| err("commit_graph_if_enabled", e))?;
        let mut graph = r.revision_graph(cache.as_ref());
        let base = r
            .merge_base_octopus_with_graph(ids, &mut graph)
            .map_err(|e| err("merge_base_octopus_with_graph", e))?;
        Ok(base.map(|id| id.to_string()))
    }

    /// The blob-merge options resolved from configuration (diff algorithm, conflict handling).
    #[napi]
    pub fn blob_merge_options(&self) -> napi::Result<BlobMergeOptionsSummary> {
        let r = thread_local(&self.inner);
        let opts = r
            .blob_merge_options()
            .map_err(|e| err("blob_merge_options", e))?;
        Ok(BlobMergeOptionsSummary {
            is_virtual_ancestor: opts.is_virtual_ancestor,
            diff_algorithm: format!("{:?}", opts.text.diff_algorithm),
            conflict: format!("{:?}", opts.text.conflict),
        })
    }

    /// Resolved options used for three-way tree merges, as driven by configuration.
    #[napi]
    pub fn tree_merge_options(&self) -> napi::Result<TreeMergeOptionsSummary> {
        let r = thread_local(&self.inner);
        // gix's `merge::tree::Options` wrapper keeps its fields private, so read the
        // plumbing-level options through the provided conversion.
        let opts: gix::merge::plumbing::tree::Options = r
            .tree_merge_options()
            .map_err(|e| err("tree_merge_options", e))?
            .into();
        Ok(TreeMergeOptionsSummary {
            rewrites_enabled: opts.rewrites.is_some(),
            blob_merge: format!("{:?}", opts.blob_merge),
            fail_on_conflict: opts.fail_on_conflict.is_some(),
            marker_size_multiplier: opts.marker_size_multiplier as u32,
            tree_conflicts: opts.tree_conflicts.map(|c| format!("{:?}", c)),
        })
    }

    /// Recursively merge multiple merge-bases into one virtual merge base commit.
    #[napi]
    pub fn virtual_merge_base(
        &self,
        merge_bases: Vec<String>,
    ) -> napi::Result<VirtualMergeBaseOutcome> {
        let r = thread_local(&self.inner);
        let ids: Vec<gix::ObjectId> = merge_bases
            .iter()
            .map(|b| parse_oid(b))
            .collect::<napi::Result<Vec<_>>>()?;
        let options = r
            .tree_merge_options()
            .map_err(|e| err("tree_merge_options", e))?;
        let outcome = r
            .virtual_merge_base(ids, options)
            .map_err(|e| err("virtual_merge_base", e))?;
        Ok(VirtualMergeBaseOutcome {
            commit_id: outcome.commit_id.to_string(),
            tree_id: outcome.tree_id.to_string(),
            virtual_merge_bases: outcome
                .virtual_merge_bases
                .into_iter()
                .map(|id| id.to_string())
                .collect(),
        })
    }

    /// Like [`virtual_merge_base`](Self::virtual_merge_base), reusing a revision graph for merge-base lookups.
    #[napi]
    pub fn virtual_merge_base_with_graph(
        &self,
        merge_bases: Vec<String>,
    ) -> napi::Result<VirtualMergeBaseOutcome> {
        let r = thread_local(&self.inner);
        let ids: Vec<gix::ObjectId> = merge_bases
            .iter()
            .map(|b| parse_oid(b))
            .collect::<napi::Result<Vec<_>>>()?;
        let options = r
            .tree_merge_options()
            .map_err(|e| err("tree_merge_options", e))?;
        let cache = r
            .commit_graph_if_enabled()
            .map_err(|e| err("commit_graph_if_enabled", e))?;
        let mut graph = r.revision_graph(cache.as_ref());
        let outcome = r
            .virtual_merge_base_with_graph(ids, &mut graph, options)
            .map_err(|e| err("virtual_merge_base", e))?;
        Ok(VirtualMergeBaseOutcome {
            commit_id: outcome.commit_id.to_string(),
            tree_id: outcome.tree_id.to_string(),
            virtual_merge_bases: outcome
                .virtual_merge_bases
                .into_iter()
                .map(|id| id.to_string())
                .collect(),
        })
    }

    /// Merge two trees using a common ancestor tree.
    ///
    /// Returns the merged tree ID, whether any conflict was encountered
    /// (including auto-resolved ones), whether any conflict is still
    /// unresolved, and details for each conflict.
    #[napi]
    pub fn merge_trees(
        &self,
        our_tree: String,
        their_tree: String,
        ancestor_tree: String,
        options: Option<MergeOptions>,
    ) -> napi::Result<MergeOutcome> {
        let r = thread_local(&self.inner);
        let ours = parse_oid(&our_tree)?;
        let theirs = parse_oid(&their_tree)?;
        let ancestor = parse_oid(&ancestor_tree)?;
        let owned = owned_labels(options.as_ref());
        let tree_options = build_tree_options("merge_trees", options.as_ref())?;
        let mut outcome = r
            .merge_trees(ancestor, ours, theirs, owned.borrowed(), tree_options)
            .map_err(|e| err("merge_trees", e))?;
        merge_outcome_to_dto(&r, &mut outcome, Some(&ancestor), "merge_tree_write")
    }

    /// Merge two commits and return the merged tree ID and conflict status.
    #[napi]
    pub fn merge_commits(
        &self,
        our_commit: String,
        their_commit: String,
        options: Option<MergeOptions>,
    ) -> napi::Result<MergeOutcome> {
        let r = thread_local(&self.inner);
        let ours = parse_oid(&our_commit)?;
        let theirs = parse_oid(&their_commit)?;
        let owned = owned_labels(options.as_ref());
        let commit_options = build_commit_options("merge_commits", options.as_ref())?;
        let mut outcome = r
            .merge_commits(ours, theirs, owned.borrowed(), commit_options)
            .map_err(|e| err("merge_commits", e))?;
        let base_tree = match r.merge_base(ours, theirs) {
            Ok(Some(id)) => r.find_commit(id).ok().and_then(|c| c.tree_id().ok().map(|t| t.detach())),
            _ => None,
        };
        merge_outcome_to_dto(
            &r,
            &mut outcome.tree_merge,
            base_tree.as_ref(),
            "merge_commit_write",
        )
    }

    /// Merge two trees as in [`merge_trees`](Self::merge_trees), then apply the
    /// conflicted entries (stages 1/2/3) to an index built from the merged tree.
    ///
    /// `unresolved` selects which conflicts count as unresolved when deciding
    /// which stages to record (`"git"`, `"forcedResolution"` or `"undecidable"`;
    /// default `"git"`). `removal_mode` decides how superseded unconflicted
    /// entries are removed: `"mark"` (flag them, index not fully consistent in
    /// memory) or `"prune"` (remove them, leaving a consistent index).
    /// With `write` the index is persisted to `<git-dir>/index`.
    #[napi]
    pub fn apply_merge_conflicts_to_index(
        &self,
        our_tree: String,
        their_tree: String,
        ancestor_tree: String,
        options: Option<MergeOptions>,
        unresolved: Option<String>,
        removal_mode: Option<String>,
        write: Option<bool>,
    ) -> napi::Result<ApplyMergeConflictsOutcome> {
        let what = "apply_merge_conflicts_to_index";
        let r = thread_local(&self.inner);
        let ours = parse_oid(&our_tree)?;
        let theirs = parse_oid(&their_tree)?;
        let ancestor = parse_oid(&ancestor_tree)?;
        let how = match unresolved.as_deref() {
            None => TreatAsUnresolved::git(),
            Some(preset) => parse_treat_as_unresolved(what, preset)?,
        };
        let removal_mode = match removal_mode.as_deref() {
            None | Some("mark") => apply_index_entries::RemovalMode::Mark,
            Some("prune") => apply_index_entries::RemovalMode::Prune,
            Some(other) => {
                return Err(err_str(
                    what,
                    format!("invalid removal_mode {other:?} (expected \"mark\" or \"prune\")"),
                ));
            }
        };
        let owned = owned_labels(options.as_ref());
        let tree_options = build_tree_options(what, options.as_ref())?;
        let mut outcome = r
            .merge_trees(ancestor, ours, theirs, owned.borrowed(), tree_options)
            .map_err(|e| err("merge_trees", e))?;
        let tree_id = outcome
            .tree
            .write()
            .map_err(|e| err("merge_tree_write", e))?;
        // The index path is the repository's own index, so writing persists there.
        let mut index = r
            .index_from_tree(&tree_id)
            .map_err(|e| err("index_from_tree", e))?;
        let changed = outcome.index_changed_after_applying_conflicts(&mut index, how, removal_mode);
        if write.unwrap_or(false) {
            index
                .write(gix::index::write::Options::default())
                .map_err(|e| err("index_write", e))?;
        }
        Ok(ApplyMergeConflictsOutcome {
            changed,
            tree_id: tree_id.to_string(),
        })
    }
}

/// The result of applying conflicted stages to an index.
#[napi(object)]
pub struct ApplyMergeConflictsOutcome {
    /// `true` if the index changed (in practice, whenever there is a conflict).
    pub changed: bool,
    /// The hash of the merged tree the index was built from.
    pub tree_id: String,
}

/// One side of a standalone three-way content merge.
#[napi(object)]
pub struct MergeFileInput {
    /// The file content for this side.
    pub content: Buffer,
    /// Repository-relative path of this side. Used as the conflict-marker label
    /// when no explicit label is given, and as the basis for the result path.
    pub path: Option<String>,
    /// Tree entry mode in octal (e.g. `"100644"`, `"100755"`). Used to derive
    /// the result mode; when omitted the result mode is left unset.
    pub mode: Option<String>,
}

/// Options for the standalone three-way blob content merge.
#[derive(Default)]
#[napi(object)]
pub struct BlobMergeOptions {
    /// Label for the ancestor side of conflict markers.
    pub ancestor_label: Option<String>,
    /// Label for the "our" side of conflict markers.
    pub our_label: Option<String>,
    /// Label for the "their" side of conflict markers.
    pub their_label: Option<String>,
    /// Conflict marker style: `"merge"` (default), `"diff3"` or `"zealousDiff3"`.
    pub style: Option<String>,
    /// Number of conflict marker characters (default 7). Must be > 0.
    pub marker_size: Option<u32>,
    /// Auto-resolve conflicts instead of keeping markers:
    /// `"ours"`, `"theirs"` or `"union"`.
    pub resolve_with: Option<String>,
    /// Diff algorithm: `"myers"` (default), `"myersMinimal"` or `"histogram"`.
    pub diff_algorithm: Option<String>,
}

/// The result of a standalone blob content merge.
#[napi(object)]
pub struct BlobMergeResult {
    /// The merged content, including conflict markers if unresolved.
    pub data: Buffer,
    /// `"complete"`, `"completeWithAutoResolvedConflict"` or `"conflict"`.
    pub resolution: String,
    /// The path the merged file should use: the first non-null path among the
    /// three inputs (ours, then theirs, then ancestor).
    pub path: Option<String>,
    /// The resulting entry mode in octal, taken from the first side (ours, then
    /// theirs) that provided one.
    pub mode: Option<String>,
}

/// Perform a standalone three-way text merge of blob contents, without
/// involving a repository. `ours`, `base` and `theirs` are the file contents
/// of each side; each may carry a `path` (used for conflict-marker labels and
/// the result path) and a `mode` (used for the result mode). This is the
/// builtin text merge driver.
#[napi]
pub fn merge_blobs(
    ours: MergeFileInput,
    base: MergeFileInput,
    theirs: MergeFileInput,
    options: Option<BlobMergeOptions>,
) -> napi::Result<BlobMergeResult> {
    let what = "merge_blobs";
    let options = options.unwrap_or_default();
    let conflict = match options.resolve_with.as_deref() {
        None => {
            let style = match options.style.as_deref() {
                None | Some("merge") => text::ConflictStyle::Merge,
                Some("diff3") => text::ConflictStyle::Diff3,
                Some("zealousDiff3") => text::ConflictStyle::ZealousDiff3,
                Some(other) => return Err(err_str(what, format!("invalid style {other:?}"))),
            };
            let marker_size = match options.marker_size {
                None => text::Conflict::DEFAULT_MARKER_SIZE,
                Some(size) => u8::try_from(size)
                    .map_err(|_| err_str(what, "marker_size must fit into a u8"))?,
            };
            let marker_size = std::num::NonZeroU8::new(marker_size)
                .ok_or_else(|| err_str(what, "marker_size must be greater than 0"))?;
            text::Conflict::Keep { style, marker_size }
        }
        Some("ours") => text::Conflict::ResolveWithOurs,
        Some("theirs") => text::Conflict::ResolveWithTheirs,
        Some("union") => text::Conflict::ResolveWithUnion,
        Some(other) => {
            return Err(err_str(
                what,
                format!(
                    "invalid resolve_with {other:?} (expected \"ours\", \"theirs\" or \"union\")"
                ),
            ));
        }
    };
    let diff_algorithm = match options.diff_algorithm.as_deref() {
        None | Some("myers") => imara_diff::Algorithm::Myers,
        Some("myersMinimal") => imara_diff::Algorithm::MyersMinimal,
        Some("histogram") => imara_diff::Algorithm::Histogram,
        Some(other) => {
            return Err(err_str(
                what,
                format!(
                    "invalid diff_algorithm {other:?} (expected \"myers\", \"myersMinimal\" or \"histogram\")"
                ),
            ));
        }
    };
    let labels = text::Labels {
        ancestor: options
            .ancestor_label
            .as_deref()
            .or(base.path.as_deref())
            .map(BStr::new),
        current: options
            .our_label
            .as_deref()
            .or(ours.path.as_deref())
            .map(BStr::new),
        other: options
            .their_label
            .as_deref()
            .or(theirs.path.as_deref())
            .map(BStr::new),
    };
    let mut out = Vec::new();
    // The merge itself tokenizes all three inputs; the initial content is unused.
    let mut input = imara_diff::InternedInput::new(&[][..], &[]);
    let resolution = text(
        &mut out,
        &mut input,
        labels,
        ours.content.as_ref(),
        base.content.as_ref(),
        theirs.content.as_ref(),
        text::Options {
            diff_algorithm,
            conflict,
        },
    );
    let result_path = ours
        .path
        .clone()
        .or(theirs.path.clone())
        .or(base.path.clone());
    let result_mode = ours.mode.clone().or(theirs.mode.clone());
    Ok(BlobMergeResult {
        data: Buffer::from(out),
        resolution: blob_resolution_string(&resolution).into(),
        path: result_path,
        mode: result_mode,
    })
}
