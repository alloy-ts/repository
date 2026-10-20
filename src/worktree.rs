use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use napi::Result;
use napi::Task;
use napi::bindgen_prelude::AsyncTask;
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};
use crate::repository::Repository;

/// A secondary worktree descriptor.
#[derive(Clone)]
#[napi(object)]
pub struct WorktreeProxy {
    pub id: String,
    pub git_dir: String,
    /// The absolute checkout location, resolved from the registration's `gitdir`
    /// file, or `null` if it cannot be read. The location might not exist.
    pub base: Option<String>,
    /// Whether the worktree cannot be pruned, moved or deleted.
    pub is_locked: bool,
    /// Whether the worktree can be pruned without an expiry grace period.
    pub is_prunable: bool,
    /// The reason for the lock, if the worktree is locked.
    pub lock_reason: Option<String>,
}

/// Map a gix worktree proxy to its NAPI object. Used as a macro because
/// [`Proxy`](gix::worktree::proxy::Proxy) is a private type that cannot be named.
macro_rules! proxy_to_napi {
    ($p:expr) => {{
        let p = &$p;
        WorktreeProxy {
            id: String::from_utf8_lossy(p.id().map_err(|e| err("worktree_id", e))?.as_ref())
                .into_owned(),
            git_dir: p.git_dir().to_string_lossy().into_owned(),
            base: p.base().ok().map(|b| b.to_string_lossy().into_owned()),
            is_locked: p.is_locked(),
            is_prunable: p.is_prunable(),
            lock_reason: p
                .lock_reason()
                .map(|r| String::from_utf8_lossy(&r).into_owned()),
        }
    }};
}

/// The worktree this repository instance is positioned in, if any.
#[napi(object)]
pub struct WorktreeInfo {
    /// The base of the work tree (checkout root).
    pub base: String,
    /// Whether this is the main worktree of a non-bare repository.
    pub is_main: bool,
    /// Whether the `.git` file or directory exists within the worktree.
    pub dot_git_exists: bool,
}

/// How to treat an existing local branch when adding a worktree.
#[napi]
pub enum WorktreeHeadKind {
    /// Check out an existing local branch.
    Attached,
    /// Check out a commit with a detached `HEAD`.
    Detached,
}

/// The amount of validation to bypass when removing a linked worktree.
#[napi]
pub enum WorktreeRemoveForce {
    /// Refuse to remove a dirty worktree or one containing initialized submodules.
    Never,
    /// Discard changes and initialized submodules, but retain locked worktrees.
    DiscardChanges,
    /// Also override a worktree lock.
    OverrideLock,
}

fn to_remove_force(force: Option<WorktreeRemoveForce>) -> gix::worktree::remove::Force {
    match force {
        Some(WorktreeRemoveForce::DiscardChanges) => gix::worktree::remove::Force::DiscardChanges,
        Some(WorktreeRemoveForce::OverrideLock) => gix::worktree::remove::Force::OverrideLock,
        _ => gix::worktree::remove::Force::Never,
    }
}

/// Worktree addition task running on the libuv thread pool.
pub struct AddWorktreeTask {
    repo: Arc<gix::ThreadSafeRepository>,
    destination: String,
    head_kind: WorktreeHeadKind,
    head_name: String,
}

impl Task for AddWorktreeTask {
    type Output = Repository;
    type JsValue = Repository;

    fn compute(&mut self) -> Result<Repository> {
        let r = thread_local(&self.repo);
        let head = match self.head_kind {
            WorktreeHeadKind::Detached => {
                gix::worktree::add::Head::Detached(parse_oid(&self.head_name)?)
            }
            WorktreeHeadKind::Attached => {
                let full = if self.head_name.starts_with("refs/") {
                    self.head_name.clone()
                } else {
                    format!("refs/heads/{}", self.head_name)
                };
                let name =
                    gix::refs::FullName::try_from(full.as_str()).map_err(|e| err("ref_name", e))?;
                gix::worktree::add::Head::Attached(name)
            }
        };
        let (repo, _outcome) = r
            .add_worktree(
                self.destination.as_str(),
                head,
                gix::progress::Discard,
                &AtomicBool::new(false),
            )
            .map_err(|e| err("add_worktree", e))?;
        // The checkout outcome summary is not representable next to the returned
        // repository handle (napi AsyncTask values cannot be tuples), so only
        // the opened worktree repository is surfaced.
        Ok(Repository {
            inner: Arc::new(repo.into()),
        })
    }

    fn resolve(&mut self, _env: napi::Env, output: Repository) -> Result<Repository> {
        Ok(output)
    }
}

/// Worktree removal task running on the libuv thread pool.
pub struct RemoveWorktreeTask {
    repo: Arc<gix::ThreadSafeRepository>,
    target: String,
    force: gix::worktree::remove::Force,
}

impl Task for RemoveWorktreeTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> Result<()> {
        let r = thread_local(&self.repo);
        r.remove_worktree(
            std::path::Path::new(&self.target),
            self.force,
            gix::progress::Discard,
        )
        .map_err(|e| err("remove_worktree", e))
    }

    fn resolve(&mut self, _env: napi::Env, output: ()) -> Result<()> {
        Ok(output)
    }
}

#[napi]
impl Repository {
    /// List secondary worktrees for this repository.
    #[napi]
    pub fn worktrees(&self) -> Result<Vec<WorktreeProxy>> {
        let r = thread_local(&self.inner);
        let proxies = r.worktrees().map_err(|e| err("worktrees", e))?;
        proxies.iter().map(|p| Ok(proxy_to_napi!(p))).collect()
    }

    /// List all worktrees including the main repository, opening each one.
    /// The main repository comes first, then the linked worktrees in iteration order.
    #[napi]
    pub fn worktrees_including_main(&self) -> Result<Vec<Repository>> {
        let r = thread_local(&self.inner);
        let mut out = Vec::new();
        for item in r
            .worktrees_including_main()
            .map_err(|e| err("worktrees_including_main", e))?
        {
            let repo = item.map_err(|e| err("worktree_repo", e))?;
            out.push(Repository {
                inner: Arc::new(repo.into()),
            });
        }
        Ok(out)
    }

    /// Return the worktree identified by `id` (a directory name under
    /// `.git/worktrees/`), or `null` if it does not exist.
    #[napi]
    pub fn worktree_proxy_by_id(&self, id: String) -> Result<Option<WorktreeProxy>> {
        let r = thread_local(&self.inner);
        let proxy = r
            .worktree_proxy_by_id(id.as_str())
            .map_err(|e| err("worktree_proxy_by_id", e))?;
        Ok(match proxy.as_ref() {
            Some(p) => Some(proxy_to_napi!(p)),
            None => None,
        })
    }

    /// Return the currently set worktree (the validated worktree base path),
    /// or `null` for a bare repository without a registered worktree.
    #[napi]
    pub fn worktree(&self) -> Option<WorktreeInfo> {
        let r = thread_local(&self.inner);
        r.worktree().map(|wt| WorktreeInfo {
            base: wt.base().to_string_lossy().into_owned(),
            is_main: wt.is_main(),
            dot_git_exists: wt.dot_git_exists(),
        })
    }

    /// Whether this repository instance is a linked worktree.
    #[napi]
    pub fn is_worktree(&self) -> bool {
        let r = thread_local(&self.inner);
        r.worktree().map(|wt| !wt.is_main()).unwrap_or(false)
    }

    /// Add and check out a linked worktree at `destination`.
    ///
    /// With `head_kind: "attached"`, `head_name` is an existing local branch
    /// (short or full name) that must not be checked out elsewhere; with
    /// `"detached"`, it is a commit id. Resolves with the opened worktree
    /// repository and the checkout outcome summary.
    #[napi]
    pub fn add_worktree(
        &self,
        destination: String,
        head_kind: WorktreeHeadKind,
        head_name: String,
    ) -> Result<AsyncTask<AddWorktreeTask>> {
        Ok(AsyncTask::new(AddWorktreeTask {
            repo: self.inner.clone(),
            destination,
            head_kind,
            head_name,
        }))
    }

    /// Resolve `target` (an absolute or relative worktree path, or a unique
    /// suffix of whole path components) and remove the linked worktree.
    ///
    /// `force` controls the safety checks: `never` (default) refuses a dirty
    /// worktree or one containing initialized submodules, `discardChanges`
    /// permits their deletion, `overrideLock` additionally overrides a lock.
    /// The main worktree cannot be removed.
    #[napi]
    pub fn remove_worktree(
        &self,
        target: String,
        force: Option<WorktreeRemoveForce>,
    ) -> Result<AsyncTask<RemoveWorktreeTask>> {
        Ok(AsyncTask::new(RemoveWorktreeTask {
            repo: self.inner.clone(),
            target,
            force: to_remove_force(force),
        }))
    }
}
