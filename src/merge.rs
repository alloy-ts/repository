use napi::Result;
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};
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

/// The result of a merge operation.
#[derive(Clone)]
#[napi(object)]
pub struct MergeOutcome {
    pub tree_id: String,
    pub conflicts: bool,
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

    /// Merge two trees using a common ancestor tree and return the merged tree ID and conflict status.
    #[napi]
    pub fn merge_trees(
        &self,
        our_tree: String,
        their_tree: String,
        ancestor_tree: String,
    ) -> Result<MergeOutcome> {
        let r = thread_local(&self.inner);
        let ours = parse_oid(&our_tree)?;
        let theirs = parse_oid(&their_tree)?;
        let ancestor = parse_oid(&ancestor_tree)?;
        let labels = gix::merge::blob::builtin_driver::text::Labels {
            ancestor: Some("ancestor".into()),
            current: Some("our".into()),
            other: Some("their".into()),
        };
        let mut outcome = r
            .merge_trees(ancestor, ours, theirs, labels, Default::default())
            .map_err(|e| err("merge_trees", e))?;
        let tree_id = outcome
            .tree
            .write()
            .map_err(|e| err("merge_tree_write", e))?
            .to_string();
        Ok(MergeOutcome {
            tree_id,
            conflicts: !outcome.conflicts.is_empty(),
        })
    }

    /// Merge two commits and return the merged tree ID and conflict status.
    #[napi]
    pub fn merge_commits(&self, our_commit: String, their_commit: String) -> Result<MergeOutcome> {
        let r = thread_local(&self.inner);
        let ours = parse_oid(&our_commit)?;
        let theirs = parse_oid(&their_commit)?;
        let labels = gix::merge::blob::builtin_driver::text::Labels {
            ancestor: Some("ancestor".into()),
            current: Some("our".into()),
            other: Some("their".into()),
        };
        let mut outcome = r
            .merge_commits(ours, theirs, labels, Default::default())
            .map_err(|e| err("merge_commits", e))?;
        let tree_id = outcome
            .tree_merge
            .tree
            .write()
            .map_err(|e| err("merge_commit_write", e))?
            .to_string();
        Ok(MergeOutcome {
            tree_id,
            conflicts: !outcome.tree_merge.conflicts.is_empty(),
        })
    }
}
