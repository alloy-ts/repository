//! Object replacement and revision history subsystem (`refs/replace/*`).
//!
//! Exposes `Replace` (aliased as `Revision`) and `ReplaceManifest` NAPI object structures.

use napi::Result;
use napi_derive::napi;

use crate::error::{err, thread_local};
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
    /// Author name of this revision.
    pub author_name: String,
    /// Author email of this revision.
    pub author_email: String,
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

        Ok(Some(Replace {
            this: RefInfo {
                name: ref_name,
                target,
            },
            prev: None,
            next: None,
            head: None,
            author_name: String::new(),
            author_email: String::new(),
            updated_at: String::new(),
        }))
    }

    /// Get the replace manifest for an artifact ID (`refs/replace/{aid}`).
    #[napi]
    pub fn get_replace_manifest(&self, aid: String) -> Result<Option<ReplaceManifest>> {
        let r = thread_local(&self.inner);
        let ref_name = format!("refs/replace/{aid}");
        let Some(rf) = r.try_find_reference(ref_name.as_str()).map_err(|e| err("try_find_reference", e))? else {
            return Ok(None);
        };
        let target = match rf.target() {
            gix::refs::TargetRef::Object(id) => id.to_string(),
            gix::refs::TargetRef::Symbolic(name) => name.to_string(),
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
            author_name: String::new(),
            author_email: String::new(),
            updated_at: String::new(),
        };

        Ok(Some(ReplaceManifest {
            id: aid,
            object_type: "blob".to_string(),
            revisions: vec![node.clone()],
            head: node.head.clone(),
            path: None,
        }))
    }
}
