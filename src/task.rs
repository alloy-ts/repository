use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use napi::Env;
use napi::Result;
use napi::Task;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};
use crate::repository::{Repository, find_git};
use crate::worktree::{StreamEntry, WorktreeHeadKind};

/// High-level task helpers, scoped to a [`Repository`].
#[napi]
pub struct TaskOps {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
}

async fn run_git_cmd(repo: &Arc<gix::ThreadSafeRepository>, args: &[&str]) -> Result<String> {
    let r = thread_local(repo);
    let cwd = r.workdir().map(|p| p.to_path_buf()).unwrap_or_else(|| r.path().to_path_buf());
    let git_bin = find_git().unwrap_or_else(|| std::path::PathBuf::from("git"));

    let output = tokio::process::Command::new(&git_bin)
        .current_dir(&cwd)
        .args(args)
        .output()
        .await
        .map_err(|e| crate::error::err_str("task", format!("failed to execute git command: {e}")))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let msg = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            format!("git command {:?} failed with status {}", args, output.status)
        };
        Err(napi::Error::from_reason(msg))
    }
}

#[napi]
impl TaskOps {
    /// Create a new branch `name` starting at `base` and switch to it.
    ///
    /// Equivalent to `git checkout -b <name> <base>`. Errors if a branch
    /// named `name` already exists.
    #[napi]
    pub async fn feature_branch(&self, name: String, base: String) -> Result<()> {
        let r = thread_local(&self.repo);
        let full_branch_name = format!("refs/heads/{name}");
        if r.try_find_reference(&full_branch_name)
            .map_err(|e| err("try_find_reference", e))?
            .is_some()
        {
            return Err(crate::error::err_str(
                "feature_branch",
                format!("branch '{name}' already exists"),
            ));
        }
        run_git_cmd(&self.repo, &["checkout", "-b", &name, &base]).await?;
        Ok(())
    }

    /// Stage every change in the working tree and commit them with `message`.
    ///
    /// Equivalent to `git add -A` followed by `git commit -m <message>`.
    /// Errors if there is nothing to commit.
    #[napi]
    pub async fn commit_all(&self, message: String) -> Result<()> {
        run_git_cmd(&self.repo, &["add", "-A"]).await?;
        run_git_cmd(&self.repo, &["commit", "-m", &message]).await?;
        Ok(())
    }

    /// Bring the current branch up to date with its upstream via rebase.
    ///
    /// Equivalent to `git pull --rebase`. Errors if no upstream is configured
    /// or the rebase has conflicts.
    #[napi]
    pub async fn sync(&self) -> Result<()> {
        run_git_cmd(&self.repo, &["pull", "--rebase"]).await?;
        Ok(())
    }

    /// Squash-merge `branch` into the current branch.
    ///
    /// Equivalent to `git merge --squash <branch>`. The changes are staged
    /// but **not** committed — callers should follow up with
    /// [`commit_all`](Self::commit_all) or a raw commit to record them.
    #[napi]
    pub async fn squash_merge(&self, branch: String) -> Result<()> {
        run_git_cmd(&self.repo, &["merge", "--squash", &branch]).await?;
        Ok(())
    }
}

#[napi]
impl Repository {
    /// High-level task compositions.
    #[napi]
    pub fn task(&self) -> TaskOps {
        TaskOps {
            repo: Arc::clone(&self.inner),
        }
    }
}

/// Worktree materialisation task running on the libuv thread pool.
pub struct StreamTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) tree_id: String,
}

impl StreamTask {
    pub fn new(repo: Arc<gix::ThreadSafeRepository>, tree_id: String) -> Self {
        Self { repo, tree_id }
    }
}

impl Task for StreamTask {
    type Output = Vec<StreamEntry>;
    type JsValue = Vec<StreamEntry>;

    fn compute(&mut self) -> Result<Vec<StreamEntry>> {
        let r = thread_local(&self.repo);
        let id = parse_oid(&self.tree_id)?;
        let (mut stream, _index) = r
            .worktree_stream(id)
            .map_err(|e| err("worktree_stream", e))?;
        let mut out = Vec::new();
        while let Some(mut entry) = stream.next_entry().map_err(|e| err("stream_entry", e))? {
            let path = String::from_utf8_lossy(entry.relative_path().as_ref()).into_owned();
            let mode = format!("{:?}", entry.mode);
            let id = entry.id.to_string();
            let size = entry.bytes_remaining().map(|n| n as f64);
            std::io::copy(&mut entry, &mut std::io::sink())
                .map_err(|e| err("stream_entry_read", e))?;
            out.push(StreamEntry {
                path,
                mode,
                id,
                size,
            });
        }
        Ok(out)
    }

    fn resolve(&mut self, _env: Env, output: Vec<StreamEntry>) -> Result<Vec<StreamEntry>> {
        Ok(output)
    }
}

/// Worktree-stream raw-bytes task running on the libuv thread pool.
pub struct StreamRawTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) tree_id: String,
}

impl StreamRawTask {
    pub fn new(repo: Arc<gix::ThreadSafeRepository>, tree_id: String) -> Self {
        Self { repo, tree_id }
    }
}

impl Task for StreamRawTask {
    type Output = Vec<u8>;
    type JsValue = Buffer;

    fn compute(&mut self) -> Result<Vec<u8>> {
        let r = thread_local(&self.repo);
        let id = parse_oid(&self.tree_id)?;
        let (stream, _index) = r
            .worktree_stream(id)
            .map_err(|e| err("worktree_stream", e))?;
        let mut read = stream.into_read();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut read, &mut bytes).map_err(|e| err("stream_encode", e))?;
        Ok(bytes)
    }

    fn resolve(&mut self, _env: Env, output: Vec<u8>) -> Result<Buffer> {
        Ok(Buffer::from(output))
    }
}

/// Worktree addition task running on the libuv thread pool.
pub struct AddWorktreeTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) destination: String,
    pub(crate) head_kind: WorktreeHeadKind,
    pub(crate) head_name: String,
}

impl AddWorktreeTask {
    pub fn new(
        repo: Arc<gix::ThreadSafeRepository>,
        destination: String,
        head_kind: WorktreeHeadKind,
        head_name: String,
    ) -> Self {
        Self {
            repo,
            destination,
            head_kind,
            head_name,
        }
    }
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
        Ok(Repository {
            inner: Arc::new(repo.into()),
        })
    }

    fn resolve(&mut self, _env: Env, output: Repository) -> Result<Repository> {
        Ok(output)
    }
}

/// Worktree removal task running on the libuv thread pool.
pub struct RemoveWorktreeTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) target: String,
    pub(crate) force: gix::worktree::remove::Force,
}

impl RemoveWorktreeTask {
    pub fn new(
        repo: Arc<gix::ThreadSafeRepository>,
        target: String,
        force: gix::worktree::remove::Force,
    ) -> Self {
        Self { repo, target, force }
    }
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

    fn resolve(&mut self, _env: Env, output: ()) -> Result<()> {
        Ok(output)
    }
}
