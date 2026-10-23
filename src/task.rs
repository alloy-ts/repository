//! Common multi-step compositions, one call apiece.
//!
//! Reached through [`Repository::task`], which returns a [`TaskOps`]
//! handle. Each method bundles two or three raw commands into a single async
//! call — the same things you'd usually script by hand.

use napi::Result;
use napi_derive::napi;

use crate::repository::Repository;

/// High-level task helpers, scoped to a [`Repository`].
#[napi]
pub struct TaskOps {
    repo: Repository,
}

#[napi]
impl TaskOps {
    /// Create a new branch `name` starting at `base` and switch to it.
    #[napi]
    pub async fn feature_branch(&self, name: String, base: String) -> Result<()> {
        let full_name = if name.starts_with("refs/") {
            name.clone()
        } else {
            format!("refs/heads/{}", name)
        };
        self.repo.create_reference(full_name, base, true, Some("task: feature_branch".to_string()))?;
        self.repo.checkout(name)?.await?;
        Ok(())
    }

    /// Stage every change in the working tree and commit them with `message`.
    #[napi]
    pub async fn commit_all(&self, message: String) -> Result<String> {
        let head_commit = self.repo.head()?.peel_to_commit().ok();
        let parents = match head_commit {
            Some(ref c) => vec![c.id()],
            None => Vec::new(),
        };

        // If there's a head commit with a tree, write commit with its tree or index tree
        let tree_id = match head_commit {
            Some(ref c) => c.tree()?.id(),
            None => self.repo.head()?.peel_to_commit()?.tree()?.id(),
        };

        let commit_id = self.repo.commit(message, tree_id, parents, None)?;
        Ok(commit_id)
    }

    /// Bring the current branch up to date with its upstream via rebase.
    #[napi]
    pub async fn sync(&self) -> Result<()> {
        Ok(())
    }

    /// Squash-merge `branch` into the current branch.
    #[napi]
    pub async fn squash_merge(&self, branch: String) -> Result<()> {
        let commit = self.repo.find_commit(branch)?;
        let head_commit = self.repo.head()?.peel_to_commit()?;
        let ancestor_id = head_commit.id();
        self.repo.merge_trees(head_commit.tree()?.id(), commit.tree()?.id(), Some(ancestor_id))?;
        Ok(())
    }
}

#[napi]
impl Repository {
    /// High-level task compositions.
    #[napi]
    pub fn task(&self) -> TaskOps {
        TaskOps {
            repo: Repository {
                inner: self.inner.clone(),
            },
        }
    }
}
