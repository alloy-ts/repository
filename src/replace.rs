use napi::Result;
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};
use crate::repository::Repository;

/// A replace/revision metadata node representing a replacement revision.
#[derive(Clone)]
#[napi(object)]
pub struct Replace {
    /// Reference string for this replacement node, e.g. `refs/replace/<oid>`.
    pub this: String,
    /// Reference string for previous replacement node, if any.
    pub prev: Option<String>,
    /// Reference string for next replacement node, if any.
    pub next: Option<String>,
    /// Reference string for latest replacement node head.
    pub head: Option<String>,
    /// Author name.
    pub author_name: String,
    /// Author email.
    pub author_email: String,
    /// Committer name.
    pub committer_name: Option<String>,
    /// Committer email.
    pub committer_email: Option<String>,
    /// ISO string or formatted timestamp of update.
    pub updated_at: String,
    /// Commit timestamp (seconds as string).
    pub commit_timestamp: Option<String>,
}

/// A revision node representing a replacement node (alias/equivalent DTO).
#[derive(Clone)]
#[napi(object)]
pub struct Revision {
    pub this: String,
    pub prev: Option<String>,
    pub next: Option<String>,
    pub head: Option<String>,
    pub author_name: String,
    pub author_email: String,
    pub committer_name: Option<String>,
    pub committer_email: Option<String>,
    pub updated_at: String,
    pub commit_timestamp: Option<String>,
}

impl From<Replace> for Revision {
    fn from(r: Replace) -> Self {
        Revision {
            this: r.this,
            prev: r.prev,
            next: r.next,
            head: r.head,
            author_name: r.author_name,
            author_email: r.author_email,
            committer_name: r.committer_name,
            committer_email: r.committer_email,
            updated_at: r.updated_at,
            commit_timestamp: r.commit_timestamp,
        }
    }
}

struct CommitMeta {
    author_name: String,
    author_email: String,
    committer_name: Option<String>,
    committer_email: Option<String>,
    timestamp: String,
}

fn extract_commit_meta(r: &gix::Repository, oid: Option<&gix::ObjectId>) -> CommitMeta {
    let commit_obj = oid
        .and_then(|id| r.find_commit(*id).ok())
        .or_else(|| r.head_commit().ok());

    if let Some(commit) = commit_obj {
        if let Ok(c) = commit.decode() {
            let author = c.author().ok();
            let committer = c.committer().ok();

            let author_name = author
                .as_ref()
                .map(|a| String::from_utf8_lossy(a.name).into_owned())
                .unwrap_or_else(|| "gitoxide".to_string());
            let author_email = author
                .as_ref()
                .map(|a| String::from_utf8_lossy(a.email).into_owned())
                .unwrap_or_else(|| "gitoxide@localhost".to_string());

            let committer_name = committer
                .as_ref()
                .map(|a| String::from_utf8_lossy(a.name).into_owned());
            let committer_email = committer
                .as_ref()
                .map(|a| String::from_utf8_lossy(a.email).into_owned());

            let timestamp = author
                .as_ref()
                .and_then(|a| a.time().ok())
                .map(|t| t.seconds.to_string())
                .unwrap_or_else(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                        .to_string()
                });

            return CommitMeta {
                author_name,
                author_email,
                committer_name,
                committer_email,
                timestamp,
            };
        }
    }

    let author = r.author().and_then(|a| a.ok());
    let committer = r.committer().and_then(|a| a.ok());
    let author_name = author
        .as_ref()
        .map(|a| a.name.to_string())
        .unwrap_or_else(|| "gitoxide".to_string());
    let author_email = author
        .as_ref()
        .map(|a| a.email.to_string())
        .unwrap_or_else(|| "gitoxide@localhost".to_string());
    let committer_name = committer.as_ref().map(|a| a.name.to_string());
    let committer_email = committer.as_ref().map(|a| a.email.to_string());
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();

    CommitMeta {
        author_name,
        author_email,
        committer_name,
        committer_email,
        timestamp,
    }
}

/// Root replacement manifest object for a stable static artifact/entry/object history.
#[derive(Clone)]
#[napi(object)]
pub struct ReplaceManifest {
    /// Stable artifact ID (the original object OID).
    pub id: String,
    /// `$ref` string matching the aid.
    pub ref_id: Option<String>,
    /// The object type: `"reference"`, `"blob"`, `"tag"`, `"commit"`, `"tree"`, or `"entry"`.
    pub type_: String,
    /// Flat array of Replace metadata nodes / revisions.
    pub revisions: Vec<Replace>,
    /// Global quick reference pointing to the latest version reference.
    pub head: Option<String>,
    /// Exists if initial object type is `"entry"`.
    pub path: Option<String>,
}

#[napi]
impl Repository {
    /// Create or retrieve a replace revision node for an object ID.
    #[napi]
    pub fn get_replace(&self, oid: String) -> Result<Option<Replace>> {
        let r = thread_local(&self.inner);
        let replace_ref_name = format!("refs/replace/{oid}");
        match r.try_find_reference(&replace_ref_name) {
            Ok(Some(_rf)) => {
                let parsed_oid = parse_oid(&oid).ok();
                let meta = extract_commit_meta(&r, parsed_oid.as_ref());

                Ok(Some(Replace {
                    this: replace_ref_name.clone(),
                    prev: None,
                    next: None,
                    head: Some(replace_ref_name),
                    author_name: meta.author_name,
                    author_email: meta.author_email,
                    committer_name: meta.committer_name,
                    committer_email: meta.committer_email,
                    updated_at: meta.timestamp.clone(),
                    commit_timestamp: Some(meta.timestamp),
                }))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(err("get_replace", e)),
        }
    }

    /// Create or update a replacement ref for `aid` pointing to `replacement_oid`.
    #[napi]
    pub fn set_replace(
        &self,
        aid: String,
        replacement_oid: String,
        type_: Option<String>,
        path: Option<String>,
    ) -> Result<Replace> {
        let r = thread_local(&self.inner);
        let ref_name = format!("refs/replace/{aid}");
        let node_ref_name = format!("refs/replace/{replacement_oid}");
        let target_id = parse_oid(&replacement_oid)?;

        let prev_oid = r.try_find_reference(&ref_name).ok().flatten().and_then(|rf| match rf.target() {
            gix::refs::TargetRef::Object(id) => Some(id.to_string()),
            _ => None,
        });

        let prev_ref = prev_oid.as_ref().map(|p| format!("refs/replace/{p}"));

        // Set head ref refs/replace/{aid} -> replacement_oid
        r.reference(
            ref_name.as_str(),
            target_id,
            gix::refs::transaction::PreviousValue::Any,
            format!("replace {aid} with {replacement_oid}"),
        )
        .map_err(|e| err("set_replace", e))?;

        // Set individual replace node ref at refs/replace/{replacement_oid} pointing to previous OID
        if replacement_oid != aid {
            let node_target = prev_oid
                .as_ref()
                .and_then(|p| parse_oid(p).ok())
                .unwrap_or(target_id);
            let _ = r.reference(
                node_ref_name.as_str(),
                node_target,
                gix::refs::transaction::PreviousValue::Any,
                format!("replace node {replacement_oid}"),
            );
        }

        let target_oid = parse_oid(&aid).ok();
        let meta = extract_commit_meta(&r, target_oid.as_ref());

        Ok(Replace {
            this: if replacement_oid != aid { node_ref_name } else { ref_name.clone() },
            prev: prev_ref,
            next: None,
            head: Some(ref_name),
            author_name: meta.author_name,
            author_email: meta.author_email,
            committer_name: meta.committer_name,
            committer_email: meta.committer_email,
            updated_at: meta.timestamp.clone(),
            commit_timestamp: Some(meta.timestamp),
        })
    }

    /// Get replace manifest for stable artifact ID (`aid`).
    #[napi]
    pub fn replace_manifest(
        &self,
        aid: String,
        type_: Option<String>,
        path: Option<String>,
    ) -> Result<ReplaceManifest> {
        let r = thread_local(&self.inner);
        let mut revisions = Vec::new();
        let head_ref = format!("refs/replace/{aid}");

        // Walk replacement chain starting from refs/replace/{aid}
        let mut current_ref = head_ref.clone();
        let mut visited = std::collections::HashSet::new();

        while let Ok(Some(rf)) = r.try_find_reference(&current_ref) {
            if !visited.insert(current_ref.clone()) {
                break;
            }
            let target_oid = match rf.target() {
                gix::refs::TargetRef::Object(id) => id.to_string(),
                _ => break,
            };

            let meta = extract_commit_meta(&r, parse_oid(&target_oid).ok().as_ref());
            let node_this = format!("refs/replace/{target_oid}");

            // The node ref at refs/replace/{target_oid} stores the previous node if chained
            let prev_node_ref = format!("refs/replace/{target_oid}");
            let prev_oid = if target_oid != aid {
                r.try_find_reference(&prev_node_ref)
                    .ok()
                    .flatten()
                    .and_then(|node_rf| match node_rf.target() {
                        gix::refs::TargetRef::Object(id) => {
                            let s = id.to_string();
                            if s != target_oid { Some(s) } else { None }
                        }
                        _ => None,
                    })
            } else {
                None
            };

            let prev_ref = prev_oid.map(|p| format!("refs/replace/{p}"));

            revisions.push(Replace {
                this: node_this,
                prev: prev_ref.clone(),
                next: None,
                head: Some(head_ref.clone()),
                author_name: meta.author_name,
                author_email: meta.author_email,
                committer_name: meta.committer_name,
                committer_email: meta.committer_email,
                updated_at: meta.timestamp.clone(),
                commit_timestamp: Some(meta.timestamp),
            });

            if let Some(ref p) = prev_ref {
                current_ref = p.clone();
            } else {
                break;
            }
        }

        let head = revisions.first().and_then(|r| r.head.clone());
        let obj_type = type_.unwrap_or_else(|| "blob".to_string());

        Ok(ReplaceManifest {
            id: aid.clone(),
            ref_id: Some(format!("refs/replace/{aid}")),
            type_: obj_type,
            revisions,
            head,
            path,
        })
    }

    /// Retrieve replace node as a `Revision` alias object.
    #[napi]
    pub fn get_revision(&self, oid: String) -> Result<Option<Revision>> {
        let rep = self.get_replace(oid)?;
        Ok(rep.map(Into::into))
    }
}
