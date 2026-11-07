use napi::bindgen_prelude::AsyncTask;
use napi::{Env, Task};
use napi_derive::napi;

use std::sync::Arc;

use gix::merge::tree::{TreatAsUnresolved, apply_index_entries};

use crate::error::{err, err_str, parse_oid, thread_local};
use crate::index::Index;
use crate::merge::{MergeOptions, build_tree_options, owned_labels};
use crate::repository::Repository;

/// The two commits involved in a revert, expressed in git2 terms
/// (`our_commit` is what the revert is applied onto; `revert_commit` is the
/// commit being undone).
#[derive(Clone)]
#[napi(object)]
pub struct RevertCommits {
    /// The commit the revert is applied onto (git2's `our_commit`). Defaults to
    /// HEAD when omitted.
    pub ours: Option<String>,
    /// The commit to revert (git2's `revert_commit`). Required.
    pub revert: String,
}

/// Options for [`Repository::revert`] / [`Repository::revert_commit`].
///
/// Mirrors git2's `RevertOptions`: `mainline` maps to git2's `mainline`, and
/// `merge` maps to git2's `merge_opts` (reusing this repo's existing
/// [`MergeOptions`] for labels, rename tracking, file/tree favor and
/// `failOnConflict`). git2's `checkout_builder` is intentionally omitted — this
/// binding never touches the worktree (see [`Repository::revert`]).
#[napi(object)]
pub struct RevertOptions {
    /// For a merge commit being reverted, the 1-based parent number treated as
    /// the mainline (mirrors `git revert -m <n>` and git2's `RevertOptions.mainline`).
    /// `0` (the default) means the first parent, which is also correct for
    /// non-merge commits.
    pub mainline: Option<u32>,
    /// Conflict-resolution options, reusing the merge API's [`MergeOptions`].
    pub merge: Option<MergeOptions>,
}

/// Compute the index that results from reverting `revert_id` onto `ours_id`.
///
/// A revert undoes `parent..revert_commit`; in three-way merge terms that is
/// `merge_trees(ancestor = revert_commit.tree, ours = our_commit.tree,
/// theirs = parent.tree)`, then the conflicted stages are written into an index
/// built from the merged tree. This mirrors git2's `Repository::revert_commit`,
/// which also returns an index (the caller then commits it).
fn compute_revert_commit(
    r: &gix::Repository,
    ours_id: &str,
    revert_id: &str,
    mainline: u32,
    merge: Option<&MergeOptions>,
) -> napi::Result<Index> {
    let ours_oid = parse_oid(ours_id)?;
    let revert_oid = parse_oid(revert_id)?;
    let revert_commit = r
        .find_commit(revert_oid)
        .map_err(|e| err("find_commit", e))?;

    // The change being undone is `parent..revert_commit`; choose that parent.
    let parents: Vec<gix::ObjectId> = revert_commit.parent_ids().map(|p| p.detach()).collect();
    let parent_oid = if mainline == 0 {
        *parents
            .first()
            .ok_or_else(|| err_str("revert", "cannot revert a commit without parents"))?
    } else {
        *parents.get((mainline - 1) as usize).ok_or_else(|| {
            err_str(
                "revert",
                "mainline is out of range for this commit's parents",
            )
        })?
    };
    let parent_commit = r
        .find_commit(parent_oid)
        .map_err(|e| err("find_commit", e))?;

    let ours_tree = r
        .find_commit(ours_oid)
        .map_err(|e| err("find_commit", e))?
        .tree()
        .map_err(|e| err("commit_tree", e))?
        .id;
    let ancestor_tree = revert_commit.tree().map_err(|e| err("commit_tree", e))?.id;
    let their_tree = parent_commit.tree().map_err(|e| err("commit_tree", e))?.id;

    let owned = owned_labels(merge);
    let tree_options = build_tree_options("revert", merge)?;
    let mut outcome = r
        .merge_trees(
            ancestor_tree,
            ours_tree,
            their_tree,
            owned.borrowed(),
            tree_options,
        )
        .map_err(|e| err("revert_merge", e))?;
    let tree_id = outcome
        .tree
        .write()
        .map_err(|e| err("revert_tree_write", e))?;
    let mut index = r
        .index_from_tree(&tree_id)
        .map_err(|e| err("index_from_tree", e))?;
    let how = TreatAsUnresolved::git();
    outcome.index_changed_after_applying_conflicts(
        &mut index,
        how,
        apply_index_entries::RemovalMode::Prune,
    );
    Ok(Index { inner: index })
}

/// Resolve `ours`: explicit id, or HEAD when omitted.
fn resolve_ours(r: &gix::Repository, ours: Option<&str>) -> napi::Result<String> {
    match ours {
        Some(id) => Ok(id.to_string()),
        None => Ok(r
            .head_commit()
            .map_err(|e| err("head_commit", e))?
            .id()
            .to_string()),
    }
}

#[napi]
impl Repository {
    /// Revert `revertCommit` onto `ourCommit` and return the resulting index
    /// (git2's `Repository::revert_commit`). When the revert produces conflicts
    /// they are recorded as stages 1/2/3 in the returned index, exactly as git
    /// would leave them — the caller typically writes a commit from this index.
    #[napi]
    pub fn revert_commit(
        &self,
        commits: RevertCommits,
        options: Option<RevertOptions>,
    ) -> napi::Result<Index> {
        let r = thread_local(&self.inner);
        let ours = resolve_ours(&r, commits.ours.as_deref())?;
        let mainline = options.as_ref().and_then(|o| o.mainline).unwrap_or(0);
        compute_revert_commit(
            &r,
            &ours,
            &commits.revert,
            mainline,
            options.as_ref().and_then(|o| o.merge.as_ref()),
        )
    }

    /// Revert `commitId` onto HEAD and return the resulting index (git2's
    /// `Repository::revert`).
    ///
    /// Unlike git2's `revert`, this binding does **not** check out the worktree;
    /// it returns the reverted index so the caller can commit it or check it out
    /// explicitly. This avoids clobbering the working tree by surprise and keeps
    /// the API composable. Use [`revert_commit`](Self::revert_commit) when you
    /// need an explicit "ours" base.
    #[napi]
    pub fn revert(&self, commit_id: String, options: Option<RevertOptions>) -> napi::Result<Index> {
        let r = thread_local(&self.inner);
        let ours = resolve_ours(&r, None)?;
        let mainline = options.as_ref().and_then(|o| o.mainline).unwrap_or(0);
        compute_revert_commit(
            &r,
            &ours,
            &commit_id,
            mainline,
            options.as_ref().and_then(|o| o.merge.as_ref()),
        )
    }

    // --- Async (libuv thread pool) variants --------------------------------

    #[napi(ts_return_type = "Promise<Index>")]
    pub fn revert_commit_async(
        &self,
        commits: RevertCommits,
        options: Option<RevertOptions>,
    ) -> napi::Result<AsyncTask<RevertCommitTask>> {
        Ok(AsyncTask::new(RevertCommitTask {
            repo: Arc::clone(&self.inner),
            commits,
            options,
        }))
    }

    #[napi(ts_return_type = "Promise<Index>")]
    pub fn revert_async(
        &self,
        commit_id: String,
        options: Option<RevertOptions>,
    ) -> napi::Result<AsyncTask<RevertTask>> {
        Ok(AsyncTask::new(RevertTask {
            repo: Arc::clone(&self.inner),
            commit_id,
            options,
        }))
    }
}

pub struct RevertCommitTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) commits: RevertCommits,
    pub(crate) options: Option<RevertOptions>,
}
impl Task for RevertCommitTask {
    type Output = Index;
    type JsValue = Index;
    fn compute(&mut self) -> napi::Result<Index> {
        let r = thread_local(&self.repo);
        let ours = resolve_ours(&r, self.commits.ours.as_deref())?;
        let mainline = self.options.as_ref().and_then(|o| o.mainline).unwrap_or(0);
        compute_revert_commit(
            &r,
            &ours,
            &self.commits.revert,
            mainline,
            self.options.as_ref().and_then(|o| o.merge.as_ref()),
        )
    }
    fn resolve(&mut self, _env: Env, output: Index) -> napi::Result<Index> {
        Ok(output)
    }
}

pub struct RevertTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) commit_id: String,
    pub(crate) options: Option<RevertOptions>,
}
impl Task for RevertTask {
    type Output = Index;
    type JsValue = Index;
    fn compute(&mut self) -> napi::Result<Index> {
        let r = thread_local(&self.repo);
        let ours = resolve_ours(&r, None)?;
        let mainline = self.options.as_ref().and_then(|o| o.mainline).unwrap_or(0);
        compute_revert_commit(
            &r,
            &ours,
            &self.commit_id,
            mainline,
            self.options.as_ref().and_then(|o| o.merge.as_ref()),
        )
    }
    fn resolve(&mut self, _env: Env, output: Index) -> napi::Result<Index> {
        Ok(output)
    }
}
