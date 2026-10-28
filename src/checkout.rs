//! Checkout operations: `git checkout` semantics exposed to TypeScript.
//!
//! Reached through [`Repository::checkout`] and [`Repository::checkout_branch`],
//! both of which offload the (disk-bound) work to libuv-thread-pool tasks. The
//! working tree is materialized by shelling out to the `git` binary (the same
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
use crate::repository::{Repository, find_git};

/// Options controlling [`Repository::checkout`], [`Repository::checkout_head`], [`Repository::checkout_index`], and [`Repository::checkout_tree`].
#[napi(object)]
pub struct CheckoutOptions {
    /// Overwrite local modifications in the working tree (like `git checkout --force`).
    pub force: Option<bool>,
    /// Limit the checkout to specific file paths or pathspecs.
    pub paths: Option<Vec<String>>,
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

/// Run `git` with `args` in `dir`, returning an error if it exits non-zero.
fn git_run(dir: &Path, args: &[String]) -> napi::Result<()> {
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

/// Materialize a merge's conflicts into the working tree without committing — the git-like
/// `onConflict: "leave"` behaviour. Runs `git merge --no-commit --no-ff <commit>`; on conflicts
/// git exits with code 1, which is the expected outcome (conflict markers + unmerged index), not
/// an error. A clean merge or any other failure still propagates. The branch ref is left unmoved.
pub(crate) fn merge_leave(r: &gix::Repository, commit: &str) -> napi::Result<()> {
    let work_dir = r
        .workdir()
        .ok_or_else(|| err_str("checkout", "bare repository has no worktree"))?;
    let git = find_git().ok_or_else(|| err_str("checkout", "git binary not found"))?;
    let output = ProcCommand::new(&git)
        .current_dir(work_dir)
        .args([
            "merge".to_string(),
            "--no-commit".to_string(),
            "--no-ff".to_string(),
            commit.to_string(),
        ])
        .output()
        .map_err(|e| err("checkout", e))?;
    if output.status.success() {
        Ok(())
    } else if output.status.code() == Some(1) {
        // Conflicts present — expected for "leave"; the working tree now holds the
        // conflict markers and the index holds the unmerged stages (1/2/3).
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
            force: options.as_ref().and_then(|o| o.force).unwrap_or(false),
        }))
    }

    /// Update files in the index and working tree to match `HEAD`.
    ///
    /// Mirrors git2's `checkout_head`.
    #[napi(js_name = "checkoutHead")]
    pub fn checkout_head(&self, options: Option<CheckoutOptions>) -> napi::Result<()> {
        let r = thread_local(&self.inner);
        let dir = r.workdir().unwrap_or_else(|| r.path());
        let mut args = vec!["checkout".to_string()];
        if options.as_ref().and_then(|o| o.force).unwrap_or(false) {
            args.push("--force".to_string());
        }
        args.push("HEAD".to_string());
        args.push("--".to_string());
        if let Some(paths) = options.as_ref().and_then(|o| o.paths.as_ref()) {
            if !paths.is_empty() {
                args.extend(paths.clone());
            } else {
                args.push(".".to_string());
            }
        } else {
            args.push(".".to_string());
        }
        git_run(dir, &args)
    }

    /// Update files in the working tree to match the content of the index.
    ///
    /// Mirrors git2's `checkout_index`.
    #[napi(js_name = "checkoutIndex")]
    pub fn checkout_index(&self, options: Option<CheckoutOptions>) -> napi::Result<()> {
        let r = thread_local(&self.inner);
        let dir = r.workdir().unwrap_or_else(|| r.path());
        let mut args = vec!["checkout-index".to_string()];
        if options.as_ref().and_then(|o| o.force).unwrap_or(false) {
            args.push("-f".to_string());
        }
        args.push("-a".to_string());
        if let Some(paths) = options.as_ref().and_then(|o| o.paths.as_ref()) {
            if !paths.is_empty() {
                args.retain(|a| a != "-a");
                args.extend(paths.clone());
            }
        }
        git_run(dir, &args)
    }

    /// Update files in the index and working tree to match a treeish revision.
    ///
    /// Mirrors git2's `checkout_tree`. `treeish` can be a tree SHA, commit SHA, or rev spec.
    #[napi(js_name = "checkoutTree")]
    pub fn checkout_tree(
        &self,
        treeish: String,
        options: Option<CheckoutOptions>,
    ) -> napi::Result<()> {
        let r = thread_local(&self.inner);
        let dir = r.workdir().unwrap_or_else(|| r.path());
        let mut args = vec!["checkout".to_string()];
        if options.as_ref().and_then(|o| o.force).unwrap_or(false) {
            args.push("--force".to_string());
        }
        args.push(treeish);
        args.push("--".to_string());
        if let Some(paths) = options.as_ref().and_then(|o| o.paths.as_ref()) {
            if !paths.is_empty() {
                args.extend(paths.clone());
            } else {
                args.push(".".to_string());
            }
        } else {
            args.push(".".to_string());
        }
        git_run(dir, &args)
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
}
