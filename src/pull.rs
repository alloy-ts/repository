use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use napi::Env;
use napi::Task;
use napi::bindgen_prelude::{AsyncTask, Function};
use napi::threadsafe_function::ThreadsafeFunction;
use napi_derive::napi;

use crate::auth::AuthOpts;
use crate::checkout::{checkout_worktree, git_merge};
use crate::error::{err, err_str, parse_oid, thread_local};
use crate::fetch::{FetchOutcome, FetchTask};
use crate::merge::MergeConflict;
use crate::progress::ProgressEvent;
use crate::remote::FetchTags;
use crate::repository::Repository;
use crate::revision::WalkOptions;
use crate::stash::StashSaveOptions;

use gix::refs::transaction::PreviousValue;

/// Options controlling [`Repository::pull`] / [`Repository::fetch`].
#[derive(Default)]
#[napi(object)]
pub struct PullOptions {
    /// Remote to pull from. Defaults to the branch's upstream remote, then `"origin"`.
    pub remote: Option<String>,
    /// Local branch to pull into. Defaults to the current `HEAD` branch.
    pub branch: Option<String>,
    /// Integration strategy after fetching: `"ff-only"`, `"ff"` (default) or `"merge"`.
    ///
    /// `rebase` is intentionally not accepted yet. When it lands, its behaviour must be driven
    /// by the repository config rather than bespoke options: read `pull.rebase` to decide whether
    /// rebase is requested, and honour the `rebase.*` settings (`rebase.merges`,
    /// `rebase.autosquash`, interaction, etc.) via [`Repository::config_string`] /
    /// [`Repository::config_snapshot`] (exposed from `config.rs`).
    pub strategy: Option<String>,
    /// Extra refspecs to fetch in addition to the remote's configured ones.
    pub refspecs: Option<Vec<String>>,
    /// Shallow fetch depth (`git fetch --depth N`). `0` or `null` means full history.
    pub depth: Option<u32>,
    /// Negotiate and report what would change, but do not modify the local repository.
    pub dry_run: Option<bool>,
    /// Reflog message prefix; defaults to the standard message.
    pub reflog_message: Option<String>,
    /// Write updated refs straight into `packed-refs` (server-side fetches).
    pub write_packed_refs_only: Option<bool>,
    /// Authenticator supplying credentials / SSH keys / agent / credential helper.
    pub auth: Option<AuthOpts>,
    /// How to fetch tags during the pull's fetch step: `"all"`, `"none"`, or `"included"`.
    /// `null` uses the remote's configured default.
    pub tags: Option<FetchTags>,
    /// Behaviour when the merge step hits unresolvable conflicts: `"abort"` (default) keeps the
    /// branch and working tree untouched and only surfaces the conflicts in the outcome;
    /// `"leave"` writes the conflicted index and conflict markers into the working tree
    /// (git-like), leaving the branch ref unmoved so the caller can resolve and commit.
    pub on_conflict: Option<String>,
}

/// Summary of a completed [`Repository::pull`].
#[napi(object)]
pub struct PullOutcome {
    /// Local branch that was pulled into.
    pub branch: Option<String>,
    /// Remote that was pulled from.
    pub remote: String,
    /// Upstream tracking ref that was fetched (e.g. `refs/remotes/origin/main`).
    pub tracking: Option<String>,
    /// The strategy that was requested.
    pub strategy: String,
    /// What happened: `"up-to-date"`, `"fast-forwarded"`, `"merged"`, or `"rejected"`.
    pub result: String,
    /// Local branch commit id before the pull.
    pub old_commit_id: Option<String>,
    /// Local branch commit id after the pull (`null` when nothing changed / rejected).
    pub new_commit_id: Option<String>,
    /// The underlying [`FetchOutcome`] describing what the fetch retrieved.
    pub fetch: FetchOutcome,
    /// Per-path conflicts when a real merge (`"merged"`) produced (auto-resolved or not) conflicts.
    pub conflicts: Option<Vec<MergeConflict>>,
    /// Commits the local branch was ahead of the upstream before the pull.
    pub ahead_before: u32,
    /// Commits the local branch was behind the upstream before the pull.
    pub behind_before: u32,
}

/// Pull and fetch operations for Git repositories.
#[napi]
impl Repository {
    /// Fetch from a named remote and integrate the result into the current branch — the
    /// `git pull` operation.
    ///
    /// The fetch updates the remote-tracking ref; integration then fast-forwards (default),
    /// merges (`strategy: "merge"` on a divergence), or is rejected (`strategy: "ff-only"` on a
    /// divergence). Dirty working trees are auto-stashed before integration and restored
    /// afterwards. Rebasing is not yet supported. Returns a [`PullOutcome`] describing whether
    /// anything changed.
    #[napi]
    pub fn pull(
        &self,
        options: Option<PullOptions>,
        progress: Option<Function<ProgressEvent, ()>>,
    ) -> napi::Result<AsyncTask<PullTask>> {
        let opts = options.unwrap_or_default();
        let progress = match progress {
            Some(f) => {
                let builder = f.build_threadsafe_function::<ProgressEvent>();
                Some(Arc::new(builder.callee_handled().build()?))
            }
            None => None,
        };
        Ok(AsyncTask::new(PullTask {
            repo: Arc::clone(&self.inner),
            remote: opts.remote,
            branch: opts.branch,
            strategy: opts.strategy.unwrap_or_else(|| "ff".to_string()),
            refspecs: opts.refspecs,
            depth: opts.depth,
            dry_run: opts.dry_run,
            reflog_message: opts.reflog_message,
            write_packed_refs_only: opts.write_packed_refs_only,
            auth: opts.auth,
            tags: opts.tags,
            on_conflict: opts.on_conflict,
            progress,
            interrupt: Arc::new(AtomicBool::new(false)),
        }))
    }
}

/// Blocking pull work, executed on the libuv thread pool by [`AsyncTask`].
pub struct PullTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) remote: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) strategy: String,
    pub(crate) refspecs: Option<Vec<String>>,
    pub(crate) depth: Option<u32>,
    pub(crate) dry_run: Option<bool>,
    pub(crate) reflog_message: Option<String>,
    pub(crate) write_packed_refs_only: Option<bool>,
    pub(crate) auth: Option<AuthOpts>,
    pub(crate) progress: Option<Arc<ThreadsafeFunction<ProgressEvent, ()>>>,
    pub(crate) interrupt: Arc<AtomicBool>,
    pub(crate) tags: Option<FetchTags>,
    pub(crate) on_conflict: Option<String>,
}

impl Task for PullTask {
    type Output = PullOutcome;
    type JsValue = PullOutcome;

    fn compute(&mut self) -> napi::Result<PullOutcome> {
        self.interrupt.store(false, Ordering::Relaxed);
        let r = thread_local(&self.repo);
        let repo = Repository {
            inner: Arc::clone(&self.repo),
        };

        // Resolve the local branch (pull requires a branch, not a detached HEAD).
        let head_name = repo
            .head_name()?
            .ok_or_else(|| err_str("pull", "cannot pull: HEAD is detached (no branch)"))?;
        let branch = self.branch.clone().unwrap_or_else(|| {
            head_name
                .strip_prefix("refs/heads/")
                .unwrap_or(&head_name)
                .to_string()
        });
        let branch_full = format!("refs/heads/{branch}");

        // Resolve the upstream tracking ref for that branch.
        let tracking = repo
            .branch_remote_tracking_ref_name(branch.clone(), None)?
            .ok_or_else(|| {
                err_str(
                    "pull",
                    format!("no upstream configured for branch '{branch}'"),
                )
            })?;

        // Determine the remote to fetch from (explicit, else the tracking ref's remote).
        let remote_name = self
            .remote
            .clone()
            .or_else(|| {
                tracking
                    .strip_prefix("refs/remotes/")
                    .and_then(|rest| rest.split('/').next())
                    .map(|s| s.to_string())
            })
            .ok_or_else(|| err_str("pull", "could not determine the remote to fetch from"))?;

        // 1. Fetch (the same plumbing as Remote::fetch).
        let mut fetch_task = FetchTask {
            repo: Arc::clone(&self.repo),
            name: Some(remote_name.clone()),
            url: None,
            refspecs: self.refspecs.clone(),
            depth: self.depth,
            dry_run: self.dry_run,
            reflog_message: self.reflog_message.clone(),
            write_packed_refs_only: self.write_packed_refs_only,
            progress: self.progress.clone(),
            interrupt: Arc::clone(&self.interrupt),
            auth: self.auth.take(),
            tags: self.tags.take(),
            prune: None,
            override_refspecs: None,
        };
        let fetch_outcome = fetch_task.compute()?;

        let head_commit_id = repo.head_id()?;
        let upstream_commit_id = repo.rev_parse_single(tracking.clone())?;

        // 2. Compute ahead / behind via revision walks.
        let ahead = repo
            .rev_walk(
                vec![head_commit_id.clone()],
                Some(WalkOptions {
                    sort: None,
                    reverse: None,
                    first_parent: None,
                    exclude: Some(vec![upstream_commit_id.clone()]),
                    glob: None,
                    range: None,
                    limit: None,
                }),
            )?
            .len() as u32;
        let behind = repo
            .rev_walk(
                vec![upstream_commit_id.clone()],
                Some(WalkOptions {
                    sort: None,
                    reverse: None,
                    first_parent: None,
                    exclude: Some(vec![head_commit_id.clone()]),
                    glob: None,
                    range: None,
                    limit: None,
                }),
            )?
            .len() as u32;

        let dry_run = self.dry_run.unwrap_or(false);
        let has_workdir = r.workdir().is_some();

        // 3. Decide the outcome without mutating anything yet.
        let (result, new_commit_id, conflicts): (&str, Option<String>, Option<Vec<MergeConflict>>) =
            if ahead == 0 && behind == 0 {
                ("up-to-date", Some(head_commit_id.clone()), None)
            } else if ahead == 0 && behind > 0 {
                // Fast-forward is possible under any sane strategy.
                ("fast-forwarded", Some(upstream_commit_id.clone()), None)
            } else if ahead > 0 && behind > 0 {
                // Diverged: fast-forward is impossible.
                match self.strategy.as_str() {
                    "ff-only" => ("rejected", Some(head_commit_id.clone()), None),
                    "ff" | "merge" => ("merged", None, None),
                    // TODO(rebase): when `strategy == "rebase"`, consult `pull.rebase` and the
                    // `rebase.*` config (rebase.merges / autosquash / interaction, ...) via the
                    // repository config — do NOT add explicit rebase options. Then either use
                    // gix's rebase or shell out to `git pull --rebase` (mirroring task.rs).
                    _ => ("rejected", Some(head_commit_id.clone()), None),
                }
            } else {
                // Local ahead of upstream: nothing to pull.
                ("up-to-date", Some(head_commit_id.clone()), None)
            };

        // 4. Apply the integration (unless dry-run or nothing to do).
        let mut applied_new_id = new_commit_id.clone();
        let mut applied_conflicts = conflicts.clone();
        // Set when conflicts are left in the working tree so we don't clobber them by
        // auto-popping the stash.
        let mut skip_stash_pop = false;
        if !dry_run {
            // Auto-stash a dirty working tree; restored at the end.
            let stash_id = if has_workdir {
                repo.save_stash(StashSaveOptions {
                    message: Some(format!("pull: auto-stash before integrating {branch}")),
                    flags: None,
                    pathspecs: None,
                    author: None,
                })?
            } else {
                None
            };

            if result == "fast-forwarded" {
                let upstream_oid = parse_oid(&upstream_commit_id)?;
                let full = gix::refs::FullName::try_from(branch_full.as_str())
                    .map_err(|e| err("pull: ref_name", e))?;
                r.reference(
                    full,
                    upstream_oid,
                    PreviousValue::MustExist,
                    format!("pull: fast-forward {branch}"),
                )
                .map_err(|e| err("pull: set_ref", e))?;
                if has_workdir {
                    checkout_worktree(&r, &branch)?;
                }
                applied_new_id = Some(upstream_commit_id.clone());
            } else if result == "merged" {
                let merged =
                    repo.merge_commits(head_commit_id.clone(), upstream_commit_id.clone(), None)?;
                if merged.has_unresolved_conflicts {
                    // Cannot write a merge commit; leave the branch as-is and surface conflicts.
                    applied_new_id = Some(head_commit_id.clone());
                    applied_conflicts = Some(merged.conflict_details);
                    // `onConflict: "leave"` — materialize the conflicted index and conflict
                    // markers into the working tree (git-like) instead of leaving it untouched.
                    // The branch ref is NOT advanced and the auto-stash is left in place for the
                    // caller to pop after resolving.
                    if self.on_conflict.as_deref() == Some("leave") && has_workdir {
                        git_merge(&r, &[upstream_commit_id.clone()], true)?;
                        skip_stash_pop = true;
                    }
                } else {
                    let new_id = repo.commit(
                        branch_full.clone(),
                        format!("Merge branch '{branch}' of {tracking}"),
                        merged.tree_id.clone(),
                        vec![head_commit_id.clone(), upstream_commit_id.clone()],
                    )?;
                    if has_workdir {
                        checkout_worktree(&r, &branch)?;
                    }
                    applied_new_id = Some(new_id);
                    applied_conflicts = if merged.conflicts {
                        Some(merged.conflict_details)
                    } else {
                        None
                    };
                }
            }
            // `up-to-date` / `rejected`: nothing to apply.

            // Restore the auto-stash, unless conflicts were left in the working tree
            // (the caller must resolve them, then pop the stash themselves, like git).
            if stash_id.is_some() && !skip_stash_pop {
                repo.pop_stash(0, None)?;
            }
        }

        Ok(PullOutcome {
            branch: Some(branch.clone()),
            remote: remote_name,
            tracking: Some(tracking.clone()),
            strategy: self.strategy.clone(),
            result: result.to_string(),
            old_commit_id: Some(head_commit_id),
            new_commit_id: applied_new_id,
            fetch: fetch_outcome,
            conflicts: applied_conflicts,
            ahead_before: ahead,
            behind_before: behind,
        })
    }

    fn resolve(&mut self, _env: Env, output: PullOutcome) -> napi::Result<PullOutcome> {
        Ok(output)
    }
}
