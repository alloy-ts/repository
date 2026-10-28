use std::sync::Arc;

use crate::error::RepositoryResult as Result;
use napi_derive::napi;

use gix::bstr::ByteSlice;
use gix::refs::transaction::PreviousValue;

use crate::commit::signature_ref;
use crate::error::{err, err_str, parse_oid, thread_local};
use crate::identity::Signature;
use crate::merge::{
    MergeOptions, MergeOutcome, build_tree_options, merge_outcome_to_dto, owned_labels,
};
use crate::repository::{Repository, lossy};

/// Options controlling an in-memory rebase.
#[napi(object)]
pub struct RebaseOptions {
    /// Tree/blob merge options applied when replaying each commit
    /// (labels, favor, rewrites, conflict handling — see `MergeOptions`).
    pub merge_options: Option<MergeOptions>,
}

/// A single planned rebase operation.
#[napi(object)]
pub struct RebaseOperation {
    /// The operation type; currently always `"pick"` (the commit is
    /// cherry-picked onto the tip).
    pub kind: String,
    /// The commit being replayed.
    pub id: String,
}

/// The result of applying one rebase operation via [`Rebase::next`].
#[napi(object)]
pub struct RebaseStepOutcome {
    /// Zero-based index of the applied operation.
    pub index: u32,
    /// The operation that was applied.
    pub operation: RebaseOperation,
    /// The merge result for this step: merged tree plus conflict details.
    pub merge: MergeOutcome,
}

/// The result of a one-shot [`Repository::rebase`].
#[napi(object)]
pub struct RebaseOutcome {
    /// `true` if every operation was replayed and the branch ref updated.
    pub completed: bool,
    /// The rebased tip (last successfully created commit), or the starting
    /// `onto` tip when nothing was applied.
    pub head_id: String,
    /// Number of operations successfully committed.
    pub applied: u32,
    /// Total number of planned operations.
    pub total: u32,
    /// The failed step when `completed` is `false` because a step produced
    /// unresolved conflicts; `null` otherwise.
    pub conflict: Option<RebaseStepOutcome>,
}

/// Internal planning result shared by the step and one-shot APIs.
struct RebasePlan {
    /// Full name of the rebased ref (e.g. `refs/heads/main`), or `None` when
    /// rebasing a detached commit; only refs are updated on finish.
    orig_head_name: Option<String>,
    /// The commit the rebase started from (the branch tip).
    orig_head_id: gix::ObjectId,
    /// The commit the operations are replayed onto.
    onto: gix::ObjectId,
    /// Commits to replay, oldest first, linearized along the first-parent
    /// chain of the branch.
    ops: Vec<gix::ObjectId>,
}

fn plan_rebase(
    r: &gix::Repository,
    what: &str,
    branch: Option<&str>,
    upstream: Option<&str>,
    onto: Option<&str>,
) -> napi::Result<RebasePlan> {
    let (orig_head_id, orig_head_name) = match branch {
        Some(spec) => {
            let id: gix::ObjectId = r.rev_parse_single(spec).map_err(|e| err(what, e))?.into();
            let name = r
                .find_reference(spec)
                .ok()
                .and_then(|rf| rf.name().as_bstr().to_str().ok().map(str::to_string));
            (id, name)
        }
        None => {
            let name = r
                .head()
                .ok()
                .and_then(|h| h.name().as_bstr().to_str().ok().map(str::to_string));
            (r.head_id().map_err(|e| err(what, e))?.into(), name)
        }
    };

    let (upstream_id, onto_id) = match upstream {
        Some(spec) => {
            let id: gix::ObjectId = r.rev_parse_single(spec).map_err(|e| err(what, e))?.into();
            let onto = match onto {
                Some(o) => parse_oid(o)?,
                None => id,
            };
            (id, onto)
        }
        None => {
            // 未指定 upstream：以 branch 与 HEAD 的 merge base 为分叉点，
            // 将分叉后的提交重放到 HEAD 上。
            let head_id: gix::ObjectId = r.head_id().map_err(|e| err(what, e))?.into();
            let base = r
                .merge_base(orig_head_id, head_id)
                .map_err(|e| err(what, e))?
                .map(|b| b.detach())
                .ok_or_else(|| err_str(what, "branch and HEAD have no merge base"))?;
            let onto = match onto {
                Some(o) => parse_oid(o)?,
                None => head_id,
            };
            (base, onto)
        }
    };

    // 与 `git rev-list --first-parent upstream..branch` 等价：隐藏 upstream
    // 可达的提交，只沿 branch 的第一父链收集待重放提交，旧→新排序。
    let platform = r
        .rev_walk([orig_head_id])
        .with_hidden([upstream_id])
        .first_parent_only();
    let mut ops: Vec<gix::ObjectId> = Vec::new();
    for info in platform.all().map_err(|e| err(what, e))? {
        let info = info.map_err(|e| err(what, e))?;
        ops.push(info.id);
    }
    ops.reverse();

    Ok(RebasePlan {
        orig_head_name,
        orig_head_id,
        onto: onto_id,
        ops,
    })
}

fn commit_tree_id(
    r: &gix::Repository,
    what: &str,
    id: gix::ObjectId,
) -> napi::Result<gix::ObjectId> {
    r.find_commit(id)
        .and_then(|c| c.tree_id())
        .map(|t| t.detach())
        .map_err(|e| err(what, e))
}

/// The tree a replayed commit is three-way merged against: its first parent's
/// tree, or the empty tree for a root commit.
fn op_base_tree(
    r: &gix::Repository,
    what: &str,
    op_id: gix::ObjectId,
) -> napi::Result<gix::ObjectId> {
    let commit = r.find_commit(op_id).map_err(|e| err(what, e))?;
    let parent = commit.decode().map_err(|e| err(what, e))?.parents().next();
    match parent {
        Some(p) => commit_tree_id(r, what, p),
        None => r
            .write_object(&gix::objs::Tree {
                entries: Vec::new(),
            })
            .map(|t| t.detach())
            .map_err(|e| err(what, e)),
    }
}

/// Replay one operation: three-way merge of the operation's tree (`theirs`)
/// onto the current tip (`ours`) against the operation's parent tree (base).
fn apply_operation(
    r: &gix::Repository,
    what: &str,
    tip: gix::ObjectId,
    op_id: gix::ObjectId,
    options: Option<&MergeOptions>,
) -> napi::Result<(gix::ObjectId, MergeOutcome)> {
    let base = op_base_tree(r, what, op_id)?;
    let ours = commit_tree_id(r, what, tip)?;
    let theirs = commit_tree_id(r, what, op_id)?;
    let owned = owned_labels(options);
    let tree_options = build_tree_options(what, options)?;
    let mut outcome = r
        .merge_trees(base, ours, theirs, owned.borrowed(), tree_options)
        .map_err(|e| err(what, e))?;
    // merge_outcome_to_dto 会先把编辑器树写入对象库，这里回读其 id。
    let merge = merge_outcome_to_dto(r, &mut outcome, Some(&base), what)?;
    let tree_id = parse_oid(&merge.tree_id)?;
    Ok((tree_id, merge))
}

/// Create the rebased commit for `op_id`: original author and message (kept
/// when `message_override` is `None`), new committer, single parent `tip`.
fn create_rebase_commit(
    r: &gix::Repository,
    what: &str,
    op_id: gix::ObjectId,
    tip: gix::ObjectId,
    tree: gix::ObjectId,
    committer: Option<Signature>,
    message_override: Option<String>,
) -> napi::Result<gix::ObjectId> {
    let commit = r.find_commit(op_id).map_err(|e| err(what, e))?;
    let decoded = commit.decode().map_err(|e| err(what, e))?;
    // 直接借用原提交的 author 签名（含原始时间），new_commit_as 需要 SignatureRef。
    let author = decoded.author().map_err(|e| err(what, e))?;
    let message = message_override.unwrap_or_else(|| lossy(decoded.message));
    let default_committer = r.committer().and_then(|res| res.ok());
    let mut committer_buf = String::new();
    let committer = match &committer {
        Some(s) => signature_ref(s, &mut committer_buf),
        None => default_committer
            .ok_or_else(|| err_str(what, "no committer configured; pass `committer` explicitly"))?,
    };
    r.new_commit_as(committer, author, message.as_str(), tree, vec![tip])
        .map(|c| c.id)
        .map_err(|e| err(what, e))
}

fn operation_dto(id: gix::ObjectId) -> RebaseOperation {
    RebaseOperation {
        kind: "pick".into(),
        id: id.to_string(),
    }
}

fn update_rebased_ref(
    r: &gix::Repository,
    what: &str,
    name: &str,
    tip: gix::ObjectId,
) -> napi::Result<()> {
    // 只有指向真实 ref 的会话才落引用；detached 会话仅返回 tip。
    if !name.starts_with("refs/") {
        return Ok(());
    }
    let label = format!("rebase: {name} onto {tip}");
    r.reference(name, tip, PreviousValue::Any, label)
        .map_err(|e| err(what, e))?;
    Ok(())
}

/// An in-memory rebase session, modeled after git2's `Rebase`.
///
/// gix has no persistent rebase state machine, so nothing is written to the
/// repository until `finish()`: replayed commits are written to the object
/// database and the branch ref is only moved by `finish()`. Conflicts stay
/// in memory — resolve them by passing a `resolvedTree` to `commit()`.
#[napi]
pub struct Rebase {
    repo: Arc<gix::ThreadSafeRepository>,
    orig_head_name: Option<String>,
    orig_head_id: gix::ObjectId,
    onto: gix::ObjectId,
    /// 待重放提交，旧→新排序。
    ops: Vec<gix::ObjectId>,
    /// 最后一次应用的操作下标；`usize::MAX` 表示尚未调用 `next()`。
    current: usize,
    /// 已重放（并提交）的最新 tip。
    tip: gix::ObjectId,
    /// 当前操作的合并树，等待 `commit()`。
    pending_tree: Option<gix::ObjectId>,
    pending_unresolved: bool,
    merge_options: Option<MergeOptions>,
    finished: bool,
}

#[napi]
impl Rebase {
    /// The number of planned rebase operations.
    #[napi(getter)]
    pub fn len(&self) -> u32 {
        self.ops.len() as u32
    }

    /// Full ref name the rebase started from, or `null` for a detached tip.
    #[napi(getter)]
    pub fn orig_head_name(&self) -> Option<String> {
        self.orig_head_name.clone()
    }

    /// The commit id the rebase started from (the branch tip).
    #[napi(getter)]
    pub fn orig_head_id(&self) -> String {
        self.orig_head_id.to_string()
    }

    /// The commit id the operations are replayed onto.
    #[napi(getter)]
    pub fn onto(&self) -> String {
        self.onto.to_string()
    }

    /// The current rebased tip (last committed replay, or `onto` initially).
    #[napi(getter)]
    pub fn tip_id(&self) -> String {
        self.tip.to_string()
    }

    /// Index of the operation returned by the last `next()`, or `null` before
    /// the first `next()`.
    #[napi]
    pub fn operation_current(&self) -> Option<u32> {
        (self.current != usize::MAX).then_some(self.current as u32)
    }

    /// Peek at the planned operation `n` (without applying it).
    #[napi]
    pub fn nth(&self, n: u32) -> napi::Result<RebaseOperation> {
        let idx = n as usize;
        if idx >= self.ops.len() {
            return Err(err_str("nth", format!("index {n} out of range")));
        }
        Ok(operation_dto(self.ops[idx]))
    }

    /// All planned operations, in order.
    #[napi]
    pub fn operations(&self) -> napi::Result<Vec<RebaseOperation>> {
        Ok(self.ops.iter().copied().map(operation_dto).collect())
    }

    /// Apply the next operation and return its merge result.
    ///
    /// The patch of the operation's commit is three-way merged onto the
    /// current tip; conflicts are reported in `merge.conflictDetails` and must
    /// be resolved before `commit()` (via `resolvedTree`).
    #[napi]
    pub fn next(&mut self) -> napi::Result<RebaseStepOutcome> {
        let what = "rebase_next";
        if self.finished {
            return Err(err_str(what, "the rebase is finished or aborted"));
        }
        if self.pending_tree.is_some() {
            return Err(err_str(
                what,
                "the previous operation is not committed yet; call commit() or abort()",
            ));
        }
        let index = self.current.wrapping_add(1);
        if index >= self.ops.len() {
            return Err(err_str(what, "no more operations to apply"));
        }
        let r = thread_local(&self.repo);
        let op_id = self.ops[index];
        let (tree_id, merge) =
            apply_operation(&r, what, self.tip, op_id, self.merge_options.as_ref())?;
        self.pending_tree = Some(tree_id);
        self.pending_unresolved = merge.has_unresolved_conflicts;
        self.current = index;
        Ok(RebaseStepOutcome {
            index: index as u32,
            operation: operation_dto(op_id),
            merge,
        })
    }

    /// Commit the current operation, advancing the rebased tip.
    ///
    /// Keeps the original author and message unless overridden. When the last
    /// `next()` reported unresolved conflicts, a `resolvedTree` must be
    /// supplied; `committer` falls back to the repository configuration.
    #[napi]
    pub fn commit(
        &mut self,
        committer: Option<Signature>,
        message: Option<String>,
        resolved_tree: Option<String>,
    ) -> napi::Result<String> {
        let what = "rebase_commit";
        if self.finished {
            return Err(err_str(what, "the rebase is finished or aborted"));
        }
        let Some(pending) = self.pending_tree else {
            return Err(err_str(what, "no operation applied; call next() first"));
        };
        let tree = match &resolved_tree {
            Some(t) => parse_oid(t)?,
            None => pending,
        };
        if self.pending_unresolved && resolved_tree.is_none() {
            return Err(err_str(
                what,
                "the current operation has unresolved conflicts; resolve them and pass the resolved tree as `resolvedTree`",
            ));
        }
        let r = thread_local(&self.repo);
        let op_id = self.ops[self.current];
        let id = create_rebase_commit(&r, what, op_id, self.tip, tree, committer, message)?;
        self.tip = id;
        self.pending_tree = None;
        self.pending_unresolved = false;
        Ok(id.to_string())
    }

    /// Discard the session. Nothing was persisted, so this merely invalidates
    /// the handle — there is no repository state to roll back.
    #[napi]
    pub fn abort(&mut self) -> napi::Result<()> {
        self.finished = true;
        self.pending_tree = None;
        self.pending_unresolved = false;
        Ok(())
    }

    /// Finish the rebase once all operations are committed, moving the branch
    /// ref to the rebased tip and returning it. A detached session skips the
    /// ref update.
    #[napi]
    pub fn finish(&mut self) -> napi::Result<String> {
        let what = "rebase_finish";
        if self.finished {
            return Err(err_str(what, "the rebase is already finished or aborted"));
        }
        if self.pending_tree.is_some() {
            return Err(err_str(what, "the current operation is not committed yet"));
        }
        if self.current.wrapping_add(1) != self.ops.len() {
            return Err(err_str(what, "not all operations have been applied"));
        }
        let r = thread_local(&self.repo);
        if let Some(name) = &self.orig_head_name {
            update_rebased_ref(&r, what, name, self.tip)?;
        }
        self.finished = true;
        Ok(self.tip.to_string())
    }
}

#[napi]
impl Repository {
    /// Returns true if a rebase is currently in progress on disk
    /// (`.git/rebase-merge` or `.git/rebase-apply` directory exists).
    #[napi]
    pub fn is_rebasing(&self) -> bool {
        let r = thread_local(&self.inner);
        let git_dir = r.git_dir();
        git_dir.join("rebase-merge").is_dir() || git_dir.join("rebase-apply").is_dir()
    }

    /// Get default todo list of operations for a rebase onto upstream.
    #[napi]
    pub fn rebase_plan(&self, upstream: String) -> napi::Result<Vec<RebaseOperation>> {
        let what = "rebase_plan";
        let r = thread_local(&self.inner);
        let plan = plan_rebase(&r, what, None, Some(&upstream), None)?;
        Ok(plan.ops.into_iter().map(operation_dto).collect())
    }

    /// Abort an in-progress disk rebase (`git rebase --abort`).
    #[napi]
    pub fn rebase_abort(&self) -> napi::Result<()> {
        let r = thread_local(&self.inner);
        let git = crate::repository::find_git()
            .ok_or_else(|| err_str("rebase_abort", "no system git binary found"))?;
        let mut cmd = std::process::Command::new(&git);
        cmd.arg("rebase").arg("--abort");
        if let Some(wd) = r.workdir() {
            cmd.current_dir(wd);
        }
        match cmd.output() {
            Ok(out) if out.status.success() => Ok(()),
            Ok(out) => Err(err_str(
                "rebase_abort",
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )),
            Err(e) => Err(err("rebase_abort", e)),
        }
    }

    /// Continue an in-progress disk rebase (`git rebase --continue`).
    #[napi]
    pub fn rebase_continue(&self) -> napi::Result<()> {
        let r = thread_local(&self.inner);
        let git = crate::repository::find_git()
            .ok_or_else(|| err_str("rebase_continue", "no system git binary found"))?;
        let mut cmd = std::process::Command::new(&git);
        cmd.arg("rebase").arg("--continue");
        if let Some(wd) = r.workdir() {
            cmd.current_dir(wd);
        }
        match cmd.output() {
            Ok(out) if out.status.success() => Ok(()),
            Ok(out) => Err(err_str(
                "rebase_continue",
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )),
            Err(e) => Err(err("rebase_continue", e)),
        }
    }

    /// Skip current commit in an in-progress disk rebase (`git rebase --skip`).
    #[napi]
    pub fn rebase_skip(&self) -> napi::Result<()> {
        let r = thread_local(&self.inner);
        let git = crate::repository::find_git()
            .ok_or_else(|| err_str("rebase_skip", "no system git binary found"))?;
        let mut cmd = std::process::Command::new(&git);
        cmd.arg("rebase").arg("--skip");
        if let Some(wd) = r.workdir() {
            cmd.current_dir(wd);
        }
        match cmd.output() {
            Ok(out) if out.status.success() => Ok(()),
            Ok(out) => Err(err_str(
                "rebase_skip",
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )),
            Err(e) => Err(err("rebase_skip", e)),
        }
    }

    /// Start an in-memory rebase session: plan the commits to replay and
    /// return a step-driven `Rebase` handle.
    ///
    /// `branch` (default HEAD) is the ref or commit to rebase; `upstream`
    /// (default: the merge base of `branch` and HEAD) bounds the commits to
    /// replay; `onto` (default: `upstream`, or HEAD when `upstream` is
    /// omitted) is the new base. Nothing touches the repository until
    /// `finish()`.
    #[napi]
    pub fn rebase_init(
        &self,
        branch: Option<String>,
        upstream: Option<String>,
        onto: Option<String>,
        options: Option<RebaseOptions>,
    ) -> napi::Result<Rebase> {
        let what = "rebase_init";
        let r = thread_local(&self.inner);
        let plan = plan_rebase(
            &r,
            what,
            branch.as_deref(),
            upstream.as_deref(),
            onto.as_deref(),
        )?;
        Ok(Rebase {
            repo: Arc::clone(&self.inner),
            orig_head_name: plan.orig_head_name,
            orig_head_id: plan.orig_head_id,
            onto: plan.onto,
            ops: plan.ops,
            current: usize::MAX,
            tip: plan.onto,
            pending_tree: None,
            pending_unresolved: false,
            merge_options: options.and_then(|o| o.merge_options),
            finished: false,
        })
    }

    /// One-shot in-memory rebase: replay all planned commits and move the
    /// branch ref, returning the new tip.
    ///
    /// Stops at the first step with unresolved conflicts — the branch ref is
    /// left untouched and the failing step is reported in `conflict`. Use
    /// `rebaseInit` for step-by-step control with manual conflict resolution.
    #[napi]
    pub fn rebase(
        &self,
        branch: Option<String>,
        upstream: Option<String>,
        onto: Option<String>,
        options: Option<RebaseOptions>,
    ) -> napi::Result<RebaseOutcome> {
        let what = "rebase";
        let r = thread_local(&self.inner);
        let plan = plan_rebase(
            &r,
            what,
            branch.as_deref(),
            upstream.as_deref(),
            onto.as_deref(),
        )?;
        let total = plan.ops.len() as u32;
        let merge_options = options.and_then(|o| o.merge_options);
        let mut tip = plan.onto;
        let mut current: usize = usize::MAX;
        while current.wrapping_add(1) < plan.ops.len() {
            let index = current.wrapping_add(1);
            let op_id = plan.ops[index];
            let (tree_id, merge) = apply_operation(&r, what, tip, op_id, merge_options.as_ref())?;
            if merge.has_unresolved_conflicts {
                return Ok(RebaseOutcome {
                    completed: false,
                    head_id: tip.to_string(),
                    applied: index as u32,
                    total,
                    conflict: Some(RebaseStepOutcome {
                        index: index as u32,
                        operation: operation_dto(op_id),
                        merge,
                    }),
                });
            }
            tip = create_rebase_commit(&r, what, op_id, tip, tree_id, None, None)?;
            current = index;
        }
        if let Some(name) = &plan.orig_head_name {
            update_rebased_ref(&r, what, name, tip)?;
        }
        Ok(RebaseOutcome {
            completed: true,
            head_id: tip.to_string(),
            applied: total,
            total,
            conflict: None,
        })
    }
}
