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
    /// ISO string or formatted timestamp of update.
    pub updated_at: String,
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
    pub updated_at: String,
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
            updated_at: r.updated_at,
        }
    }
}

fn default_author_meta(r: &gix::Repository) -> (String, String, String) {
    let author = r.author().and_then(|a| a.ok());
    let author_name = author
        .as_ref()
        .map(|a| a.name.to_string())
        .unwrap_or_else(|| "gitoxide".to_string());
    let author_email = author
        .as_ref()
        .map(|a| a.email.to_string())
        .unwrap_or_else(|| "gitoxide@localhost".to_string());
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();
    (author_name, author_email, ts)
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
                let author = r.author().and_then(|a| a.ok());
                let author_name = author.as_ref().map(|a| a.name.to_string()).unwrap_or_else(|| "gitoxide".to_string());
                let author_email = author.as_ref().map(|a| a.email.to_string()).unwrap_or_else(|| "gitoxide@localhost".to_string());

                Ok(Some(Replace {
                    this: replace_ref_name.clone(),
                    prev: None,
                    next: None,
                    head: Some(replace_ref_name),
                    author_name,
                    author_email,
                    updated_at: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                        .to_string(),
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

        let prev_ref = r.try_find_reference(&ref_name).ok().flatten().map(|rf| match rf.target() {
            gix::refs::TargetRef::Object(id) => format!("refs/replace/{}", id),
            _ => String::new(),
        });

        // Set head ref refs/replace/{aid}
        r.reference(
            ref_name.as_str(),
            target_id,
            gix::refs::transaction::PreviousValue::Any,
            format!("replace {aid} with {replacement_oid}"),
        )
        .map_err(|e| err("set_replace", e))?;

        // Also set individual replace node ref at refs/replace/{replacement_oid}
        if replacement_oid != aid {
            let _ = r.reference(
                node_ref_name.as_str(),
                target_id,
                gix::refs::transaction::PreviousValue::Any,
                format!("replace node {replacement_oid}"),
            );
        }

        let is_entry = type_.as_deref() == Some("entry");
        let (author_name, author_email, updated_at) = if is_entry {
            if let Ok(head_commit) = r.head_commit() {
                if let Ok(c) = head_commit.decode() {
                    let author = c.author().ok();
                    let name = author
                        .as_ref()
                        .map(|a| String::from_utf8_lossy(a.name).into_owned())
                        .unwrap_or_else(|| "gitoxide".to_string());
                    let email = author
                        .as_ref()
                        .map(|a| String::from_utf8_lossy(a.email).into_owned())
                        .unwrap_or_else(|| "gitoxide@localhost".to_string());
                    let ts = author
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
                    (name, email, ts)
                } else {
                    default_author_meta(&r)
                }
            } else {
                default_author_meta(&r)
            }
        } else {
            default_author_meta(&r)
        };

        Ok(Replace {
            this: if replacement_oid != aid { node_ref_name } else { ref_name.clone() },
            prev: prev_ref,
            next: None,
            head: Some(ref_name),
            author_name,
            author_email,
            updated_at,
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
        let mut revisions = Vec::new();
        if let Ok(Some(n)) = self.get_replace(aid.clone()) {
            revisions.push(n);
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
