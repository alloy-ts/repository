//! Checkout operations: `git checkout` semantics exposed to TypeScript.
//!
//! Reached through [`Repository::checkout`] and [`Repository::checkout_branch`],
//! both of which offload the (disk-bound) work to libuv-thread-pool tasks. The
//! working tree is materialised by shelling out to the `git` binary (the same
//! approach the rest of this crate uses for `commit`/`push`/`gpg`), since gix's
//! own worktree checkout depends on a `gix_worktree_state`/`gix_filter` pair
//! whose re-exported types are skewed in this crate's dependency graph.

use std::path::Path;
use std::process::Command as ProcCommand;
use std::sync::Arc;

use gix;
use napi::Task;
use napi::bindgen_prelude::AsyncTask;
use napi_derive::napi;

use crate::error::{err, err_str, thread_local};
use crate::index::Index;
use crate::repository::{Repository, find_git};

/// Options controlling [`Repository::checkout`].
#[napi(object)]
pub struct CheckoutOptions {
    /// Overwrite local modifications in the working tree (like `git checkout --force`).
    pub force: Option<bool>,
}

/// Options controlling [`Repository::checkout_branch`].
#[napi(object)]
pub struct CheckoutBranchOptions {
    /// Create the branch if it does not exist (`git checkout -b`).
    pub create: Option<bool>,
    /// Overwrite local modifications in the working tree.
    pub force: Option<bool>,
    /// Base commit/rev for `create` (defaults to `HEAD`).
    pub start_point: Option<String>,
}

/// Options controlling [`Repository::checkout_head`], [`Repository::checkout_tree`],
/// and [`Repository::checkout_index`].
#[napi(object)]
pub struct CheckoutPathsOptions {
    /// Overwrite local modifications in the working tree (like `git checkout --force`).
    pub force: Option<bool>,
    /// Restrict the operation to these pathspecs (git pathspec syntax). Omit to
    /// affect the whole working tree.
    pub paths: Option<Vec<String>>,
}

/// Run `git` with `args` in `dir`, returning an error if it exits non-zero.
pub(crate) fn git_run(dir: &Path, args: &[String]) -> napi::Result<()> {
    let git = find_git().ok_or_else(|| err_str("checkout", "git binary not found"))?;
    let output = ProcCommand::new(&git)
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|e| err("checkout", e))?;
    if !output.status.success() {
        return Err(err_str(
            "checkout",
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        )
        .into());
    }
    Ok(())
}

/// Resolve `spec` to a commit id via `git rev-parse --verify <spec>`.
fn git_rev_parse(dir: &Path, spec: &str) -> napi::Result<String> {
    let git = find_git().ok_or_else(|| err_str("checkout", "git binary not found"))?;
    let output = ProcCommand::new(&git)
        .current_dir(dir)
        .args(["rev-parse", "--verify", spec])
        .output()
        .map_err(|e| err("checkout", e))?;
    if !output.status.success() {
        return Err(err_str(
            "checkout",
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Sync the working tree to `rev` (typically the current branch after a ref update) by shelling
/// out to `git reset --hard`. Used by `pull` to refresh disk after fast-forward / merge
/// integration. Bare repositories have no worktree and return an error.
pub(crate) fn checkout_worktree(r: &gix::Repository, rev: &str) -> napi::Result<()> {
    let work_dir = r
        .workdir()
        .ok_or_else(|| err_str("checkout", "bare repository has no worktree"))?;
    git_run(
        work_dir,
        &["reset".to_string(), "--hard".to_string(), rev.to_string()],
    )
}

/// Run `git merge --no-commit [--no-ff] <commits…>` to materialise a merge into the working tree
/// and index, leaving the repository in a *merging state* (like `git merge`). A non-zero exit of
/// 1 (git's "conflicts present" code) is treated as the expected outcome, not an error; any other
/// failure propagates. `no_ff` forces a merge even when a fast-forward is possible (used by
/// `pull`'s `onConflict: "leave"`); the standalone `merge` binding passes `false` so git
/// fast-forwards when it can.
pub(crate) fn git_merge(r: &gix::Repository, commits: &[String], no_ff: bool) -> napi::Result<()> {
    let work_dir = r
        .workdir()
        .ok_or_else(|| err_str("checkout", "bare repository has no worktree"))?;
    let git = find_git().ok_or_else(|| err_str("checkout", "git binary not found"))?;
    let mut args = vec!["merge".to_string(), "--no-commit".to_string()];
    if no_ff {
        args.push("--no-ff".to_string());
    }
    args.extend(commits.iter().cloned());
    let output = ProcCommand::new(&git)
        .current_dir(work_dir)
        .args(args)
        .output()
        .map_err(|e| err("checkout", e))?;
    if output.status.success() {
        Ok(())
    } else if output.status.code() == Some(1) {
        // Conflicts present — expected; the working tree now holds the conflict markers
        // and the index holds the unmerged stages (1/2/3).
        Ok(())
    } else {
        Err(err_str(
            "checkout",
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        )
        .into())
    }
}

/// Checkout task running on libuv thread pool.
pub struct CheckoutTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) target: String,
    pub(crate) force: bool,
}

impl Task for CheckoutTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> napi::Result<String> {
        let r = thread_local(&self.repo);
        let dir = r.workdir().unwrap_or_else(|| r.path());
        let oid = git_rev_parse(dir, &self.target)?;

        let mut args = vec!["checkout".to_string()];
        if self.force {
            args.push("--force".to_string());
        }
        args.push(self.target.clone());
        git_run(dir, &args)?;

        Ok(oid)
    }

    fn resolve(&mut self, _env: napi::Env, output: String) -> napi::Result<String> {
        Ok(output)
    }
}

/// Branch-checkout task running on libuv thread pool.
pub struct CheckoutBranchTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) name: String,
    pub(crate) create: bool,
    pub(crate) force: bool,
    pub(crate) start_point: Option<String>,
}

impl Task for CheckoutBranchTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> napi::Result<String> {
        let r = thread_local(&self.repo);
        let dir = r.workdir().unwrap_or_else(|| r.path());

        // `git checkout <name>` auto-creates a local branch from
        // `origin/<name>` when the local one is missing; `-b` forces creation
        // from `start_point` (default HEAD). Mirrors `git checkout [--create] <name>`.
        let mut args = vec!["checkout".to_string()];
        if self.force {
            args.push("--force".to_string());
        }
        if self.create {
            args.push("-b".to_string());
        }
        args.push(self.name.clone());
        if self.create {
            if let Some(start_point) = &self.start_point {
                args.push(start_point.clone());
            }
        }
        git_run(dir, &args)?;

        Ok(format!("refs/heads/{}", self.name))
    }

    fn resolve(&mut self, _env: napi::Env, output: String) -> napi::Result<String> {
        Ok(output)
    }
}

/// `git checkout [--force] HEAD -- <paths|".">` on the libuv thread pool.
/// Refreshes the working tree (and index) to the current `HEAD` without moving
/// `HEAD` or switching branches.
pub struct CheckoutHeadTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) force: bool,
    pub(crate) paths: Vec<String>,
}

impl Task for CheckoutHeadTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        let dir = r.workdir().unwrap_or_else(|| r.path());
        let mut args = vec!["checkout".to_string()];
        if self.force {
            args.push("--force".to_string());
        }
        args.push("HEAD".to_string());
        if self.paths.is_empty() {
            args.push(".".to_string());
        } else {
            args.push("--".to_string());
            args.extend(self.paths.iter().cloned());
        }
        git_run(dir, &args)
    }

    fn resolve(&mut self, _env: napi::Env, _output: ()) -> napi::Result<()> {
        Ok(())
    }
}

/// `git checkout [--force] <treeish> -- <paths|".">` on the libuv thread pool.
/// Materialises an arbitrary tree/commit into the working tree and index without
/// changing `HEAD` or the current branch.
pub struct CheckoutTreeTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) treeish: String,
    pub(crate) force: bool,
    pub(crate) paths: Vec<String>,
}

impl Task for CheckoutTreeTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        let dir = r.workdir().unwrap_or_else(|| r.path());
        let mut args = vec!["checkout".to_string()];
        if self.force {
            args.push("--force".to_string());
        }
        args.push(self.treeish.clone());
        if self.paths.is_empty() {
            args.push(".".to_string());
        } else {
            args.push("--".to_string());
            args.extend(self.paths.iter().cloned());
        }
        git_run(dir, &args)
    }

    fn resolve(&mut self, _env: napi::Env, _output: ()) -> napi::Result<()> {
        Ok(())
    }
}

/// `git checkout-index [-f] [-a | -- <paths>]` on the libuv thread pool.
/// Syncs the (already on-disk) index into the working tree without touching
/// `HEAD`.
pub struct CheckoutIndexTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) force: bool,
    pub(crate) paths: Vec<String>,
}

impl Task for CheckoutIndexTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        let dir = r.workdir().unwrap_or_else(|| r.path());
        let mut args = vec!["checkout-index".to_string()];
        if self.force {
            args.push("--force".to_string());
        }
        if self.paths.is_empty() {
            args.push("-a".to_string());
        } else {
            args.push("--".to_string());
            args.extend(self.paths.iter().cloned());
        }
        git_run(dir, &args)
    }

    fn resolve(&mut self, _env: napi::Env, _output: ()) -> napi::Result<()> {
        Ok(())
    }
}

#[napi]
impl Repository {
    /// Checkout a rev (commit id, branch, or tag) asynchronously, updating
    /// `HEAD` (detached for a non-branch rev), the index, and the working tree.
    /// Resolves to the checked-out commit id. Equivalent to
    /// `git checkout [--force] <rev>`.
    #[napi(ts_return_type = "Promise<String>")]
    pub fn checkout(
        &self,
        target: String,
        options: Option<CheckoutOptions>,
    ) -> napi::Result<AsyncTask<CheckoutTask>> {
        Ok(AsyncTask::new(CheckoutTask {
            repo: Arc::clone(&self.inner),
            target,
            force: options.and_then(|o| o.force).unwrap_or(false),
        }))
    }

    /// Checkout a branch by name asynchronously, updating `HEAD`, the index, and
    /// the working tree.
    ///
    /// If the branch exists locally it is checked out. With `create`, a new
    /// branch is made from `start_point` (default `HEAD`). Without `create`, an
    /// `origin/<name>` remote-tracking branch is used to create a local tracking
    /// branch (git's default `checkout <name>` behaviour). Resolves to the full
    /// ref name (`refs/heads/<name>`).
    #[napi(ts_return_type = "Promise<String>")]
    pub fn checkout_branch(
        &self,
        name: String,
        options: Option<CheckoutBranchOptions>,
    ) -> napi::Result<AsyncTask<CheckoutBranchTask>> {
        let opts = options.unwrap_or(CheckoutBranchOptions {
            create: None,
            force: None,
            start_point: None,
        });
        Ok(AsyncTask::new(CheckoutBranchTask {
            repo: Arc::clone(&self.inner),
            name,
            create: opts.create.unwrap_or(false),
            force: opts.force.unwrap_or(false),
            start_point: opts.start_point,
        }))
    }

    /// Restore the working tree and index to the content of the current `HEAD`,
    /// without switching branches or detaching `HEAD`. Equivalent to
    /// `git checkout [--force] HEAD -- [paths]`. Resolves to `undefined`.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn checkout_head(
        &self,
        options: Option<CheckoutPathsOptions>,
    ) -> napi::Result<AsyncTask<CheckoutHeadTask>> {
        let opts = options.unwrap_or(CheckoutPathsOptions {
            force: None,
            paths: None,
        });
        Ok(AsyncTask::new(CheckoutHeadTask {
            repo: Arc::clone(&self.inner),
            force: opts.force.unwrap_or(false),
            paths: opts.paths.unwrap_or_default(),
        }))
    }

    /// Materialise an arbitrary tree/commit (`treeish`) into the working tree and
    /// index, without changing `HEAD` or the current branch. Equivalent to
    /// `git checkout [--force] <treeish> -- [paths]`. Resolves to `undefined`.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn checkout_tree(
        &self,
        treeish: String,
        options: Option<CheckoutPathsOptions>,
    ) -> napi::Result<AsyncTask<CheckoutTreeTask>> {
        let opts = options.unwrap_or(CheckoutPathsOptions {
            force: None,
            paths: None,
        });
        Ok(AsyncTask::new(CheckoutTreeTask {
            repo: Arc::clone(&self.inner),
            treeish,
            force: opts.force.unwrap_or(false),
            paths: opts.paths.unwrap_or_default(),
        }))
    }

    /// Sync the index into the working tree without touching `HEAD`, mirroring
    /// git2's `checkout_index`. If `index` is provided, it is flushed to the
    /// repository's on-disk index first (so its in-memory entries are
    /// materialised); otherwise the repository's current index is used. `paths`
    /// restricts the operation. Equivalent to
    /// `git checkout-index [-f] [-a | -- <paths>]`. Resolves to `undefined`.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn checkout_index(
        &self,
        index: Option<&mut Index>,
        options: Option<CheckoutPathsOptions>,
    ) -> napi::Result<AsyncTask<CheckoutIndexTask>> {
        if let Some(idx) = index {
            // Flush the in-memory index to the repository's on-disk index so
            // `git checkout-index` reads exactly these entries.
            idx.write(None)?;
        }
        let opts = options.unwrap_or(CheckoutPathsOptions {
            force: None,
            paths: None,
        });
        Ok(AsyncTask::new(CheckoutIndexTask {
            repo: Arc::clone(&self.inner),
            force: opts.force.unwrap_or(false),
            paths: opts.paths.unwrap_or_default(),
        }))
    }
}
