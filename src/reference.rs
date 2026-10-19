use std::sync::Arc;

use napi::Result;
use napi_derive::napi;

use crate::error::{err, thread_local};
use crate::object::{build_commit, build_tree, Commit, Signature, Tree};

fn lossy(b: &gix::bstr::BStr) -> String {
    String::from_utf8_lossy(b).into_owned()
}

pub(crate) fn signature_ref_to_dto(s: &gix::actor::SignatureRef) -> Signature {
    let (time, timezone_offset) = s
        .time()
        .ok()
        .map(|t| (t.seconds, t.offset))
        .unwrap_or((0, 0));
    Signature {
        name: String::from_utf8_lossy(s.name).into_owned(),
        email: String::from_utf8_lossy(s.email).into_owned(),
        time,
        timezone_offset,
    }
}

/// A single reflog entry recording a reference update.
#[derive(Clone)]
#[napi(object)]
pub struct ReflogEntry {
    pub previous_oid: String,
    pub new_oid: String,
    pub committer: Signature,
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

    /// Read the reflog entries for this reference.
    #[napi]
    pub fn reflog(&self) -> Result<Vec<ReflogEntry>> {
        let repo = thread_local(&self.repo);
        let reference = repo
            .find_reference(&self.name)
            .map_err(|e| err("find_reference", e))?;
        let mut platform = reference.log_iter();
        let mut out = Vec::new();
        if let Ok(Some(log)) = platform.all() {
            for entry in log {
                let entry = entry.map_err(|e| err("reflog entry", e))?;
                let committer = signature_ref_to_dto(&entry.signature);
                out.push(ReflogEntry {
                    previous_oid: entry.previous_oid.to_string(),
                    new_oid: entry.new_oid.to_string(),
                    committer,
                    message: lossy(entry.message),
                });
            }
        }
        Ok(out)
    }

    /// Whether a reflog exists for this reference.
    #[napi]
    pub fn reflog_exists(&self) -> Result<bool> {
        let repo = thread_local(&self.repo);
        let reference = repo
            .find_reference(&self.name)
            .map_err(|e| err("find_reference", e))?;
        reference.log_exists().map_err(|e| err("reflog_exists", e))
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
        let tree_id = repo
            .head_tree_id()
            .map_err(|e| err("head_tree_id", e))?;
        build_tree(&self.repo, tree_id)
    }

    /// The peeled id HEAD points to, if any.
    #[napi]
    pub fn id(&self) -> Option<String> {
        self.id.clone()
    }

    /// Read the reflog entries for HEAD.
    #[napi]
    pub fn reflog(&self) -> Result<Vec<ReflogEntry>> {
        let repo = thread_local(&self.repo);
        let head = repo.head().map_err(|e| err("head", e))?;
        let mut platform = head.log_iter();
        let mut out = Vec::new();
        if let Ok(Some(log)) = platform.all() {
            for entry in log {
                let entry = entry.map_err(|e| err("reflog entry", e))?;
                let committer = signature_ref_to_dto(&entry.signature);
                out.push(ReflogEntry {
                    previous_oid: entry.previous_oid.to_string(),
                    new_oid: entry.new_oid.to_string(),
                    committer,
                    message: lossy(entry.message),
                });
            }
        }
        Ok(out)
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
