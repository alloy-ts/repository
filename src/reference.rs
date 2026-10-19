use std::sync::Arc;

use gix::config::AsBStr;
use napi::Result;
use napi_derive::napi;

use crate::error::{err, thread_local};
use crate::object::{Commit, Tree, build_commit, build_tree};

fn lossy(b: impl AsRef<[u8]>) -> String {
    String::from_utf8_lossy(b.as_ref()).into_owned()
}

/// A single reflog entry recorded for a reference.
#[derive(Clone, Debug)]
#[napi(object)]
pub struct ReflogEntry {
    pub previous_id: String,
    pub new_id: String,
    pub committer_name: String,
    pub committer_email: String,
    pub committer_time: i64,
    pub message: String,
}

/// A git reference (branch, tag ref, or symbolic ref) snapshot.
#[napi]
pub struct Reference {
    #[napi(readonly)]
    pub name: String,
    /// The direct object id this reference points at, if it is not symbolic.
    #[napi(readonly)]
    pub target: Option<String>,
    /// The fully peeled object id (following symbolic refs and annotated tags).
    #[napi(readonly)]
    pub peeled: Option<String>,
    repo: Arc<gix::ThreadSafeRepository>,
}

#[napi]
impl Reference {
    /// The object id this reference ultimately resolves to (peeled).
    #[napi]
    pub fn peel(&self) -> Result<String> {
        let repo = thread_local(&self.repo);
        let mut reference = repo
            .find_reference(&self.name)
            .map_err(|e| err("find_reference", e))?;
        let id = reference
            .peel_to_id()
            .map_err(|e| err("peel_to_id", e))?
            .to_string();
        Ok(id)
    }

    /// Check whether a reflog exists for this reference.
    #[napi]
    pub fn has_log(&self) -> Result<bool> {
        let repo = thread_local(&self.repo);
        let reference = repo
            .find_reference(&self.name)
            .map_err(|e| err("find_reference", e))?;
        reference.log_exists().map_err(|e| err("log_exists", e))
    }

    /// Read reflog entries for this reference (newest entries first).
    #[napi]
    pub fn log(&self) -> Result<Vec<ReflogEntry>> {
        let repo = thread_local(&self.repo);
        let reference = repo
            .find_reference(&self.name)
            .map_err(|e| err("find_reference", e))?;

        let mut platform = reference.log_iter();
        let mut entries = Vec::new();

        if let Ok(Some(iter)) = platform.rev() {
            for entry in iter {
                if let Ok(entry) = entry {
                    entries.push(ReflogEntry {
                        previous_id: entry.previous_oid.to_string(),
                        new_id: entry.new_oid.to_string(),
                        committer_name: lossy(entry.signature.name.as_bstr()),
                        committer_email: lossy(entry.signature.email.as_bstr()),
                        committer_time: entry.signature.time.seconds,
                        message: lossy(entry.message.as_bstr()),
                    });
                }
            }
        }
        Ok(entries)
    }

    /// Delete this reference. Fails if it changed since it was observed.
    #[napi]
    pub fn delete(&self) -> Result<()> {
        let repo = thread_local(&self.repo);
        let reference = repo
            .find_reference(&self.name)
            .map_err(|e| err("find_reference", e))?;
        reference.delete().map_err(|e| err("delete reference", e))?;
        Ok(())
    }
}

pub(crate) fn build_reference(
    repo: &Arc<gix::ThreadSafeRepository>,
    mut reference: gix::Reference,
) -> Result<Reference> {
    let name = lossy(reference.name().as_bstr());
    let target = reference.target().try_id().map(|id| id.to_string());
    let peeled = reference.peel_to_id().ok().map(|id| id.to_string());
    Ok(Reference {
        name,
        target,
        peeled,
        repo: Arc::clone(repo),
    })
}

/// The repository's `HEAD`.
#[napi]
pub struct Head {
    #[napi(readonly)]
    pub name: String,
    /// The symbolic referent name (e.g. `refs/heads/main`), if HEAD is not detached.
    #[napi(readonly)]
    pub referent: Option<String>,
    /// The peeled object id HEAD points to, if the repository is not unborn.
    #[napi(readonly)]
    pub id: Option<String>,
    repo: Arc<gix::ThreadSafeRepository>,
}

#[napi]
impl Head {
    /// The commit `HEAD` ultimately points to (errors if unborn or detached-to-non-commit).
    #[napi]
    pub fn commit(&self) -> Result<Commit> {
        let repo = thread_local(&self.repo);
        let commit = repo.head_commit().map_err(|e| err("head_commit", e))?;
        build_commit(&self.repo, commit)
    }

    /// The tree `HEAD` points to (via its commit).
    #[napi]
    pub fn tree(&self) -> Result<Tree> {
        let repo = thread_local(&self.repo);
        let tree_id = repo.head_tree_id().map_err(|e| err("head_tree_id", e))?;
        build_tree(&self.repo, tree_id)
    }

    /// The peeled id HEAD points to, if any.
    #[napi]
    pub fn id(&self) -> Option<String> {
        self.id.clone()
    }

    /// Read reflog entries for `HEAD` (newest entries first).
    #[napi]
    pub fn log(&self) -> Result<Vec<ReflogEntry>> {
        let repo = thread_local(&self.repo);
        let head_ref = repo
            .find_reference("HEAD")
            .map_err(|e| err("find_reference", e))?;
        let mut platform = head_ref.log_iter();
        let mut entries = Vec::new();

        if let Ok(Some(iter)) = platform.rev() {
            for entry in iter {
                if let Ok(entry) = entry {
                    entries.push(ReflogEntry {
                        previous_id: entry.previous_oid.to_string(),
                        new_id: entry.new_oid.to_string(),
                        committer_name: lossy(entry.signature.name.as_bstr()),
                        committer_email: lossy(entry.signature.email.as_bstr()),
                        committer_time: entry.signature.time.seconds,
                        message: lossy(entry.message.as_bstr()),
                    });
                }
            }
        }
        Ok(entries)
    }
}

pub(crate) fn build_head(repo: &Arc<gix::ThreadSafeRepository>, head: gix::Head) -> Result<Head> {
    let referent = head.referent_name().map(|n| lossy(n.as_bstr()));
    let id = head.id().map(|id| id.to_string());
    Ok(Head {
        name: "HEAD".to_string(),
        referent,
        id,
        repo: Arc::clone(repo),
    })
}
