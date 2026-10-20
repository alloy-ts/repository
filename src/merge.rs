use napi::Result;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use gix::bstr::BStr;
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

/// A conflict encountered during a tree merge, including auto-resolved ones.
#[derive(Clone)]
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
}

struct OwnedLabels {
    ancestor: String,
    current: String,
    other: String,
}

fn owned_labels(options: Option<&MergeOptions>) -> OwnedLabels {
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
    fn borrowed(&self) -> text::Labels<'_> {
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
fn build_tree_options(
    what: &str,
    options: Option<&MergeOptions>,
) -> napi::Result<gix::merge::tree::Options> {
    let mut facade = gix::merge::tree::Options::default();
    if let Some(o) = options {
        if let Some(rewrites) = o.rewrites {
            facade = facade.with_rewrites(rewrites.then(gix::diff::Rewrites::default));
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

fn conflict_to_dto(conflict: &gix::merge::tree::Conflict, how: TreatAsUnresolved) -> MergeConflict {
    let (ours, theirs) = conflict.changes_in_resolution();
    let [base, our, their] = conflict.entries();
    let entry_dto = |entry: &Option<gix::merge::plumbing::tree::ConflictIndexEntry>| {
        entry.as_ref().map(|e| MergeConflictEntry {
            mode: mode_string(e.mode),
            id: e.id.to_string(),
        })
    };

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
    let (merged_blob_id, merged_blob_resolution) = merged_blob
        .map(|merged| {
            (
                merged.merged_blob_id.to_string(),
                blob_resolution_string(&merged.resolution).to_string(),
            )
        })
        .unzip();

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
    }
}

/// Convert a merge outcome into the JS-facing [`MergeOutcome`], writing the
/// unwritten editor tree into the object database first.
fn merge_outcome_to_dto(
    outcome: &mut gix::merge::tree::Outcome<'_>,
    what: &str,
) -> napi::Result<MergeOutcome> {
    // The standard (git) criteria for what still needs manual resolution.
    let how = TreatAsUnresolved::git();
    let conflict_details = outcome
        .conflicts
        .iter()
        .map(|c| conflict_to_dto(c, how))
        .collect();
    Ok(MergeOutcome {
        tree_id: outcome.tree.write().map_err(|e| err(what, e))?.to_string(),
        conflicts: !outcome.conflicts.is_empty(),
        has_unresolved_conflicts: outcome.has_unresolved_conflicts(how),
        failed_on_first_unresolved_conflict: outcome.failed_on_first_unresolved_conflict,
        conflict_details,
    })
}

#[napi]
impl Repository {
    /// The merge base of two commits, if any.
    #[napi]
    pub fn merge_base(&self, one: String, two: String) -> Result<Option<String>> {
        let r = thread_local(&self.inner);
        let base = r
            .merge_base(parse_oid(&one)?, parse_oid(&two)?)
            .map_err(|e| err("merge_base", e))?;
        Ok(base.map(|id| id.to_string()))
    }

    /// The (octopus) merge base of multiple commits, if any.
    #[napi]
    pub fn merge_base_octopus(&self, commits: Vec<String>) -> Result<Option<String>> {
        let r = thread_local(&self.inner);
        let ids: Vec<gix::ObjectId> = commits
            .iter()
            .map(|c| parse_oid(c))
            .collect::<Result<Vec<_>>>()?;
        let base = r
            .merge_base_octopus(ids)
            .map_err(|e| err("merge_base_octopus", e))?;
        Ok(base.map(|id| id.to_string()))
    }

    /// The merge base of two commits, building the revision graph on demand (uses the commit-graph if enabled).
    #[napi]
    pub fn merge_base_with_graph(&self, one: String, two: String) -> Result<Option<String>> {
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
    pub fn merge_bases_many(&self, one: String, others: Vec<String>) -> Result<Vec<String>> {
        let r = thread_local(&self.inner);
        let one_id = parse_oid(&one)?;
        let others_ids: Vec<gix::ObjectId> = others
            .iter()
            .map(|o| parse_oid(o))
            .collect::<Result<Vec<_>>>()?;
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
    ) -> Result<Vec<String>> {
        let r = thread_local(&self.inner);
        let one_id = parse_oid(&one)?;
        let others_ids: Vec<gix::ObjectId> = others
            .iter()
            .map(|o| parse_oid(o))
            .collect::<Result<Vec<_>>>()?;
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
    pub fn merge_base_octopus_with_graph(&self, commits: Vec<String>) -> Result<Option<String>> {
        let r = thread_local(&self.inner);
        let ids: Vec<gix::ObjectId> = commits
            .iter()
            .map(|c| parse_oid(c))
            .collect::<Result<Vec<_>>>()?;
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
    pub fn blob_merge_options(&self) -> Result<BlobMergeOptionsSummary> {
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
    pub fn tree_merge_options(&self) -> Result<TreeMergeOptionsSummary> {
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
    pub fn virtual_merge_base(&self, merge_bases: Vec<String>) -> Result<VirtualMergeBaseOutcome> {
        let r = thread_local(&self.inner);
        let ids: Vec<gix::ObjectId> = merge_bases
            .iter()
            .map(|b| parse_oid(b))
            .collect::<Result<Vec<_>>>()?;
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
    ) -> Result<VirtualMergeBaseOutcome> {
        let r = thread_local(&self.inner);
        let ids: Vec<gix::ObjectId> = merge_bases
            .iter()
            .map(|b| parse_oid(b))
            .collect::<Result<Vec<_>>>()?;
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
    ) -> Result<MergeOutcome> {
        let r = thread_local(&self.inner);
        let ours = parse_oid(&our_tree)?;
        let theirs = parse_oid(&their_tree)?;
        let ancestor = parse_oid(&ancestor_tree)?;
        let owned = owned_labels(options.as_ref());
        let tree_options = build_tree_options("merge_trees", options.as_ref())?;
        let mut outcome = r
            .merge_trees(ancestor, ours, theirs, owned.borrowed(), tree_options)
            .map_err(|e| err("merge_trees", e))?;
        merge_outcome_to_dto(&mut outcome, "merge_tree_write")
    }

    /// Merge two commits and return the merged tree ID and conflict status.
    #[napi]
    pub fn merge_commits(
        &self,
        our_commit: String,
        their_commit: String,
        options: Option<MergeOptions>,
    ) -> Result<MergeOutcome> {
        let r = thread_local(&self.inner);
        let ours = parse_oid(&our_commit)?;
        let theirs = parse_oid(&their_commit)?;
        let owned = owned_labels(options.as_ref());
        let commit_options = build_commit_options("merge_commits", options.as_ref())?;
        let mut outcome = r
            .merge_commits(ours, theirs, owned.borrowed(), commit_options)
            .map_err(|e| err("merge_commits", e))?;
        merge_outcome_to_dto(&mut outcome.tree_merge, "merge_commit_write")
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
    ) -> Result<ApplyMergeConflictsOutcome> {
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
}

/// Perform a standalone three-way text merge of blob contents, without
/// involving a repository. `ours`, `base` and `theirs` are the file contents
/// of each side. This is the builtin text merge driver.
#[napi]
pub fn merge_blobs(
    ours: Buffer,
    base: Buffer,
    theirs: Buffer,
    options: Option<BlobMergeOptions>,
) -> Result<BlobMergeResult> {
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
        ancestor: options.ancestor_label.as_deref().map(BStr::new),
        current: options.our_label.as_deref().map(BStr::new),
        other: options.their_label.as_deref().map(BStr::new),
    };
    let mut out = Vec::new();
    // The merge itself tokenizes all three inputs; the initial content is unused.
    let mut input = imara_diff::InternedInput::new(&[][..], &[]);
    let resolution = text(
        &mut out,
        &mut input,
        labels,
        ours.as_ref(),
        base.as_ref(),
        theirs.as_ref(),
        text::Options {
            diff_algorithm,
            conflict,
        },
    );
    Ok(BlobMergeResult {
        data: Buffer::from(out),
        resolution: blob_resolution_string(&resolution).into(),
    })
}
