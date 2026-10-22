use std::sync::Arc;
use napi::Result;
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};
use crate::repository::Repository;

/// Commit metadata associated with a replaced object or commit.
#[napi(object)]
#[derive(Clone, Debug, Default)]
pub struct CommitMetadata {
    pub author_name: Option<String>,
    pub author_email: Option<String>,
    pub committer_name: Option<String>,
    pub committer_email: Option<String>,
    pub commit_timestamp: Option<f64>,
}

/// An individual replace node / revision.
#[napi(object, js_name = "Revision")]
#[derive(Clone, Debug)]
pub struct Replace {
    pub original_oid: String,
    pub replacement_oid: String,
    pub object_type: Option<String>,
    pub metadata: Option<CommitMetadata>,
}

/// A collection or manifest of git replace references.
#[napi]
#[derive(Clone, Debug, Default)]
pub struct ReplaceManifest {
    repo: Option<Arc<gix::ThreadSafeRepository>>,
    canonical_oid: String,
    replaces: Vec<Replace>,
}

#[napi]
impl ReplaceManifest {
    #[napi(constructor)]
    pub fn new(canonical_oid: Option<String>, replaces: Option<Vec<Replace>>) -> Self {
        Self {
            repo: None,
            canonical_oid: canonical_oid.unwrap_or_default(),
            replaces: replaces.unwrap_or_default(),
        }
    }

    #[napi(getter)]
    pub fn canonical_oid(&self) -> String {
        self.canonical_oid.clone()
    }

    #[napi]
    pub fn add(&mut self, replace: Replace) {
        if self.canonical_oid.is_empty() {
            self.canonical_oid = replace.original_oid.clone();
        }
        self.replaces.push(replace);
    }

    #[napi(getter)]
    pub fn replaces(&self) -> Vec<Replace> {
        self.replaces.clone()
    }

    #[napi(getter)]
    pub fn length(&self) -> u32 {
        self.replaces.len() as u32
    }

    #[napi(getter)]
    pub fn latest(&self) -> Option<Replace> {
        self.replaces.last().cloned()
    }
}

impl ReplaceManifest {
    pub fn with_repo(mut self, repo: Arc<gix::ThreadSafeRepository>) -> Self {
        self.repo = Some(repo);
        self
    }
}

fn fetch_commit_metadata(r: &gix::Repository, commit_id: gix::ObjectId) -> Option<CommitMetadata> {
    let commit = r.find_commit(commit_id).ok()?;
    let author = commit.author().ok()?;
    let committer = commit.committer().ok()?;

    Some(CommitMetadata {
        author_name: Some(author.name.to_string()),
        author_email: Some(author.email.to_string()),
        committer_name: Some(committer.name.to_string()),
        committer_email: Some(committer.email.to_string()),
        commit_timestamp: committer.time().ok().map(|t| t.seconds as f64),
    })
}

#[napi]
impl Repository {
    /// Inspect an object id or file entry and get its commit metadata if part of a commit.
    #[napi]
    pub fn get_commit_metadata(&self, oid: String) -> Result<Option<CommitMetadata>> {
        let r = thread_local(&self.inner);
        let id = parse_oid(&oid)?;
        let obj = match r.find_object(id) {
            Ok(o) => o,
            Err(_) => return Ok(None),
        };

        if obj.kind == gix::object::Kind::Commit {
            return Ok(fetch_commit_metadata(&r, id));
        }

        if let Ok(head_ref) = r.head_commit() {
            let head_id = head_ref.id;
            return Ok(fetch_commit_metadata(&r, head_id));
        }

        Ok(None)
    }

    /// Read the replace manifest for an OID at refs/replace/{oid}, following replacement chains.
    #[napi]
    pub fn replace_manifest(&self, canonical_oid: String) -> Result<ReplaceManifest> {
        let r = thread_local(&self.inner);
        let mut manifest = ReplaceManifest::new(Some(canonical_oid.clone()), None)
            .with_repo(self.inner.clone());

        let mut curr = canonical_oid.clone();
        let mut visited = std::collections::HashSet::new();

        while visited.insert(curr.clone()) {
            let ref_name = format!("refs/replace/{curr}");
            let reference = match r.find_reference(&ref_name) {
                Ok(rf) => rf,
                Err(_) => break,
            };

            let next_oid = match reference.target() {
                gix::refs::TargetRef::Object(oid) => oid.to_string(),
                gix::refs::TargetRef::Symbolic(target) => {
                    let s = target.as_bstr().to_string();
                    s.strip_prefix("refs/replace/").unwrap_or(&s).to_string()
                }
            };

            let obj_type = r
                .find_object(parse_oid(&curr)?)
                .map(|o| format!("{:?}", o.kind).to_lowercase())
                .unwrap_or_else(|_| "blob".to_string());
            let meta = self.get_commit_metadata(curr.clone())?;

            manifest.add(Replace {
                original_oid: curr.clone(),
                replacement_oid: next_oid.clone(),
                object_type: Some(obj_type),
                metadata: meta,
            });

            curr = next_oid;
        }

        Ok(manifest)
    }

    /// Create or append a replacement for an OID at refs/replace/{canonical_oid}.
    #[napi]
    pub fn replace_object(
        &self,
        original_oid: String,
        replacement_oid: String,
        object_type: Option<String>,
    ) -> Result<ReplaceManifest> {
        let r = thread_local(&self.inner);

        // Find the tail of the existing replace chain starting at original_oid
        let mut tail = original_oid.clone();
        let mut visited = std::collections::HashSet::new();

        while visited.insert(tail.clone()) {
            let ref_name = format!("refs/replace/{tail}");
            let reference = match r.find_reference(&ref_name) {
                Ok(rf) => rf,
                Err(_) => break,
            };

            let next = match reference.target() {
                gix::refs::TargetRef::Object(oid) => oid.to_string(),
                gix::refs::TargetRef::Symbolic(target) => {
                    let s = target.as_bstr().to_string();
                    s.strip_prefix("refs/replace/").unwrap_or(&s).to_string()
                }
            };
            tail = next;
        }

        let ref_name = format!("refs/replace/{tail}");
        let target_oid = parse_oid(&replacement_oid)?;

        r.reference(
            ref_name.as_str(),
            target_oid,
            gix::refs::transaction::PreviousValue::Any,
            format!("replace {tail} with {replacement_oid}"),
        )
        .map_err(|e| err("replace_object", e))?;

        let _ = object_type; // Object type is inferred during manifest traversal if needed

        self.replace_manifest(original_oid)
    }
}
