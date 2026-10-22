//! Object replacement and revision history subsystem (`refs/replace/*`).
//!
//! Exposes `Replace` (aliased as `Revision`) and `ReplaceManifest` NAPI object structures.

use napi::Result;
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};
use crate::repository::Repository;

/// A lightweight reference summary for replace node references.
#[napi(object)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefInfo {
    /// Full reference name, e.g. `refs/replace/1234...`.
    pub name: String,
    /// Target object ID (40-hex string).
    pub target: String,
}

/// An individual replace/revision node at `refs/replace/{oid}`.
#[napi(object)]
#[derive(Clone, Debug)]
pub struct Replace {
    /// The reference pointing to this replacement object.
    pub this: RefInfo,
    /// Previous replacement node reference in the history chain, if any.
    pub prev: Option<RefInfo>,
    /// Next replacement node reference in the history chain, if any.
    pub next: Option<RefInfo>,
    /// Quick reference to the latest (head) replacement node.
    pub head: Option<RefInfo>,
    /// Author name of this revision or commit entry.
    pub author_name: String,
    /// Author email of this revision or commit entry.
    pub author_email: String,
    /// Committer name if this revision is associated with a commit/entry.
    pub committer_name: Option<String>,
    /// Committer email if this revision is associated with a commit/entry.
    pub committer_email: Option<String>,
    /// Commit timestamp (unix seconds) if associated with a commit/entry.
    pub commit_timestamp: Option<f64>,
    /// ISO-8601 string representation of the update date.
    pub updated_at: String,
}

/// Alias `Revision` to `Replace` per specifications.
pub type Revision = Replace;

/// Root replacement manifest for a stable static artifact/entry/object history.
#[napi(object)]
#[derive(Clone, Debug)]
pub struct ReplaceManifest {
    /// Stable artifact ID (the initial/old object OID).
    pub id: String,
    /// Object type: `"reference"`, `"blob"`, `"tag"`, `"commit"`, `"tree"`, or `"entry"`.
    #[napi(js_name = "type")]
    pub object_type: String,
    /// Flat array of Replace (Revision) metadata nodes.
    pub revisions: Vec<Replace>,
    /// Quick reference to the latest version reference.
    pub head: Option<RefInfo>,
    /// Optional file path (present if object type is `"entry"`).
    pub path: Option<String>,
}

struct CommitMeta {
    author_name: String,
    author_email: String,
    committer_name: String,
    committer_email: String,
    commit_timestamp: f64,
}

fn find_commit_meta_for_oid(
    r: &gix::Repository,
    target_oid: gix::ObjectId,
    path_hint: Option<&str>,
) -> Option<CommitMeta> {
    if let Ok(commit) = r.find_commit(target_oid) {
        if let (Ok(author), Ok(committer)) = (commit.author(), commit.committer()) {
            let timestamp = commit.time().map(|t| t.seconds as f64).unwrap_or(0.0);
            return Some(CommitMeta {
                author_name: String::from_utf8_lossy(author.name).into_owned(),
                author_email: String::from_utf8_lossy(author.email).into_owned(),
                committer_name: String::from_utf8_lossy(committer.name).into_owned(),
                committer_email: String::from_utf8_lossy(committer.email).into_owned(),
                commit_timestamp: timestamp,
            });
        }
    }

    let head = r.head_commit().ok()?;
    let mut current = head.id().detach();
    loop {
        let commit = r.find_commit(current).ok()?;
        if let Ok(tree) = commit.tree() {
            let matched = if let Some(p) = path_hint {
                tree.lookup_entry_by_path(std::path::Path::new(p)).ok().flatten().is_some()
            } else {
                tree.decode().ok().map(|t| t.entries.iter().any(|e| e.oid == target_oid)).unwrap_or(false)
            };

            if matched {
                if let (Ok(author), Ok(committer)) = (commit.author(), commit.committer()) {
                    let timestamp = commit.time().map(|t| t.seconds as f64).unwrap_or(0.0);
                    return Some(CommitMeta {
                        author_name: String::from_utf8_lossy(author.name).into_owned(),
                        author_email: String::from_utf8_lossy(author.email).into_owned(),
                        committer_name: String::from_utf8_lossy(committer.name).into_owned(),
                        committer_email: String::from_utf8_lossy(committer.email).into_owned(),
                        commit_timestamp: timestamp,
                    });
                }
            }
        }

        match commit.parent_ids().next() {
            Some(parent) => current = parent.detach(),
            None => break,
        }
    }

    None
}

#[napi]
impl Repository {
    /// Get the replacement node for a single replacement object OID (`refs/replace/{replaceOid}`).
    #[napi]
    pub fn get_replace_node(&self, replace_oid: String) -> Result<Option<Replace>> {
        let r = thread_local(&self.inner);
        let ref_name = format!("refs/replace/{replace_oid}");
        let Some(rf) = r.try_find_reference(ref_name.as_str()).map_err(|e| err("try_find_reference", e))? else {
            return Ok(None);
        };
        let target = match rf.target() {
            gix::refs::TargetRef::Object(id) => id.to_string(),
            gix::refs::TargetRef::Symbolic(name) => name.to_string(),
        };

        let target_oid = parse_oid(&target)?;
        let meta = find_commit_meta_for_oid(&r, target_oid, None);

        Ok(Some(Replace {
            this: RefInfo {
                name: ref_name,
                target,
            },
            prev: None,
            next: None,
            head: None,
            author_name: meta.as_ref().map(|m| m.author_name.clone()).unwrap_or_default(),
            author_email: meta.as_ref().map(|m| m.author_email.clone()).unwrap_or_default(),
            committer_name: meta.as_ref().map(|m| m.committer_name.clone()),
            committer_email: meta.as_ref().map(|m| m.committer_email.clone()),
            commit_timestamp: meta.as_ref().map(|m| m.commit_timestamp),
            updated_at: String::new(),
        }))
    }

    /// Get the replace manifest for an artifact ID (`refs/replace/{aid}`).
    #[napi]
    pub fn get_replace_manifest(&self, aid: String, path: Option<String>) -> Result<Option<ReplaceManifest>> {
        let r = thread_local(&self.inner);
        let ref_name = format!("refs/replace/{aid}");
        let Some(rf) = r.try_find_reference(ref_name.as_str()).map_err(|e| err("try_find_reference", e))? else {
            return Ok(None);
        };
        let target = match rf.target() {
            gix::refs::TargetRef::Object(id) => id.to_string(),
            gix::refs::TargetRef::Symbolic(name) => name.to_string(),
        };

        let target_oid = parse_oid(&target)?;
        let meta = find_commit_meta_for_oid(&r, target_oid, path.as_deref());

        let aid_oid = parse_oid(&aid).ok();
        let object_type = if path.is_some() {
            "entry".to_string()
        } else if let Some(obj) = aid_oid.and_then(|id| r.find_object(id).ok()) {
            match obj.kind {
                gix::object::Kind::Blob => "blob".to_string(),
                gix::object::Kind::Tree => "tree".to_string(),
                gix::object::Kind::Commit => "commit".to_string(),
                gix::object::Kind::Tag => "tag".to_string(),
            }
        } else {
            "blob".to_string()
        };

        let node = Replace {
            this: RefInfo {
                name: ref_name,
                target: target.clone(),
            },
            prev: None,
            next: None,
            head: Some(RefInfo {
                name: format!("refs/replace/{target}"),
                target: target.clone(),
            }),
            author_name: meta.as_ref().map(|m| m.author_name.clone()).unwrap_or_default(),
            author_email: meta.as_ref().map(|m| m.author_email.clone()).unwrap_or_default(),
            committer_name: meta.as_ref().map(|m| m.committer_name.clone()),
            committer_email: meta.as_ref().map(|m| m.committer_email.clone()),
            commit_timestamp: meta.as_ref().map(|m| m.commit_timestamp),
            updated_at: String::new(),
        };

        Ok(Some(ReplaceManifest {
            id: aid,
            object_type,
            revisions: vec![node.clone()],
            head: node.head.clone(),
            path,
        }))
    }
}
