use chrono::Utc;
use napi::Result;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;
use serde::{Deserialize, Serialize};

use crate::error::{err, parse_oid, thread_local};
use crate::repository::Repository;

/// An individual replacement revision node for an artifact.
#[derive(Clone, Serialize, Deserialize)]
#[napi(object, js_name = "Revision")]
pub struct Replace {
    #[napi(js_name = "this")]
    #[serde(rename = "this")]
    pub this_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub prev: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub next: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub head: Option<String>,
    #[napi(js_name = "authorName")]
    #[serde(rename = "authorName")]
    pub author_name: String,
    #[napi(js_name = "authorEmail")]
    #[serde(rename = "authorEmail")]
    pub author_email: String,
    #[napi(js_name = "committerName")]
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub committer_name: Option<String>,
    #[napi(js_name = "committerEmail")]
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub committer_email: Option<String>,
    #[napi(js_name = "commitTimestamp")]
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub commit_timestamp: Option<String>,
    pub updated_at: String,
}

/// The root replace manifest for a stable artifact history.
#[derive(Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct ReplaceManifest {
    #[napi(js_name = "$ref")]
    #[serde(rename = "$ref")]
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub path: Option<String>,
    pub r#type: String,
    pub head: String,
    pub revisions: Vec<Replace>,
}

fn is_blob_in_head_tree(r: &gix::Repository, oid: gix::ObjectId) -> bool {
    if let Ok(head_commit) = r.head_commit() {
        if let Ok(tree_id) = head_commit.tree_id() {
            if let Ok(tree) = r.find_tree(tree_id) {
                if let Ok(decoded) = tree.decode() {
                    return decoded.entries.iter().any(|e| e.oid == oid);
                }
            }
        }
    }
    false
}

fn detect_object_type(r: &gix::Repository, oid: gix::ObjectId, has_path: bool) -> String {
    if has_path || is_blob_in_head_tree(r, oid) {
        return "entry".to_string();
    }
    match r.find_object(oid) {
        Ok(obj) => match obj.kind {
            gix::object::Kind::Commit => "commit".to_string(),
            gix::object::Kind::Tree => "tree".to_string(),
            gix::object::Kind::Blob => "blob".to_string(),
            gix::object::Kind::Tag => "tag".to_string(),
        },
        Err(_) => "blob".to_string(),
    }
}

#[napi]
impl Repository {
    /// Replace an existing object with new content and record a new revision node.
    #[napi]
    pub fn replace(
        &self,
        original_oid: String,
        new_content: Buffer,
        path: Option<String>,
    ) -> Result<ReplaceManifest> {
        let r = thread_local(&self.inner);
        let orig_id = parse_oid(&original_oid)?;

        // 1. Write the new object content
        let new_oid = r
            .write_blob(new_content.to_vec())
            .map_err(|e| err("write_blob", e))?
            .detach();
        let new_oid_str = new_oid.to_string();

        let object_type = detect_object_type(&r, orig_id, path.is_some());

        // 2. Fetch or initialize existing manifest
        let manifest_ref = format!("refs/replace/{original_oid}");
        let mut manifest: ReplaceManifest = match r.find_reference(&manifest_ref) {
            Ok(reference) => {
                let target_id = reference.target().id().to_owned();
                let blob = r.find_blob(target_id).map_err(|e| err("find_blob", e))?;
                serde_json::from_slice(&blob.data)
                    .map_err(|e| err("parse_manifest", e))
                    .unwrap_or_else(|_| ReplaceManifest {
                        id: original_oid.clone(),
                        path: path.clone(),
                        r#type: object_type.clone(),
                        head: original_oid.clone(),
                        revisions: vec![],
                    })
            }
            Err(_) => ReplaceManifest {
                id: original_oid.clone(),
                path: path.clone(),
                r#type: object_type,
                head: original_oid.clone(),
                revisions: vec![],
            },
        };

        let prev_head = if manifest.revisions.is_empty() {
            None
        } else {
            Some(manifest.head.clone())
        };

        // 3. Construct author/committer metadata from repo defaults or HEAD commit
        let mut author_name = "gitoxide".to_string();
        let mut author_email = "gitoxide@localhost".to_string();
        let mut committer_name: Option<String> = None;
        let mut committer_email: Option<String> = None;
        let mut commit_timestamp: Option<String> = None;

        if let Ok(head_commit) = r.head_commit() {
            if let Ok(decoded) = head_commit.decode() {
                if let Ok(a) = decoded.author() {
                    author_name = String::from_utf8_lossy(a.name).into_owned();
                    author_email = String::from_utf8_lossy(a.email).into_owned();
                }
                if let Ok(c) = decoded.committer() {
                    committer_name = Some(String::from_utf8_lossy(c.name).into_owned());
                    committer_email = Some(String::from_utf8_lossy(c.email).into_owned());
                    if let Ok(t) = c.time() {
                        commit_timestamp = Some(t.seconds.to_string());
                    }
                }
            }
        }

        let node = Replace {
            this_id: new_oid_str.clone(),
            prev: prev_head.clone(),
            next: None,
            head: Some(new_oid_str.clone()),
            author_name,
            author_email,
            committer_name,
            committer_email,
            commit_timestamp,
            updated_at: Utc::now().to_rfc3339(),
        };

        // Link previous node's `next` pointer if present
        if let Some(ref prev_oid) = prev_head {
            if let Some(last_node) = manifest.revisions.last_mut() {
                last_node.next = Some(new_oid_str.clone());
            }
            // Update individual node at `refs/replace/{prev_oid}`
            let prev_node_ref = format!("refs/replace/{prev_oid}");
            if let Ok(reference) = r.find_reference(&prev_node_ref) {
                let target_id = reference.target().id().to_owned();
                if let Ok(blob) = r.find_blob(target_id) {
                    if let Ok(mut prev_node) = serde_json::from_slice::<Replace>(&blob.data) {
                        prev_node.next = Some(new_oid_str.clone());
                        if let Ok(updated_bytes) = serde_json::to_vec(&prev_node) {
                            if let Ok(updated_blob_id) = r.write_blob(updated_bytes) {
                                let _ = r.reference(
                                    prev_node_ref.as_str(),
                                    updated_blob_id.detach(),
                                    gix::refs::transaction::PreviousValue::Any,
                                    "update prev replace node next link",
                                );
                            }
                        }
                    }
                }
            }
        }

        // 4. Store individual revision node blob at refs/replace/{newOid}
        let node_bytes = serde_json::to_vec(&node).map_err(|e| err("serialize_node", e))?;
        let node_blob_id = r
            .write_blob(node_bytes)
            .map_err(|e| err("write_node_blob", e))?
            .detach();
        let node_ref = format!("refs/replace/{new_oid_str}");
        r.reference(
            node_ref.as_str(),
            node_blob_id,
            gix::refs::transaction::PreviousValue::Any,
            "record replace node",
        )
        .map_err(|e| err("write_node_ref", e))?;

        // 5. Append node to manifest revisions and update manifest head
        manifest.revisions.push(node);
        manifest.head = new_oid_str.clone();

        // 6. Write manifest blob and reference at refs/replace/{aid}
        let manifest_bytes =
            serde_json::to_vec(&manifest).map_err(|e| err("serialize_manifest", e))?;
        let manifest_blob_id = r
            .write_blob(manifest_bytes)
            .map_err(|e| err("write_manifest_blob", e))?
            .detach();

        r.reference(
            manifest_ref.as_str(),
            manifest_blob_id,
            gix::refs::transaction::PreviousValue::Any,
            "update replace manifest",
        )
        .map_err(|e| err("write_manifest_ref", e))?;

        // 7. Update objects pointers (`refs/objects/{origOid}` and `refs/objects/{newOid}`)
        let orig_obj_ref = format!("refs/objects/{original_oid}");
        let new_obj_ref = format!("refs/objects/{new_oid_str}");

        let _ = r.reference(
            orig_obj_ref.as_str(),
            new_oid,
            gix::refs::transaction::PreviousValue::Any,
            "point object ref to latest replace content",
        );

        let _ = r.reference(
            new_obj_ref.as_str(),
            new_oid,
            gix::refs::transaction::PreviousValue::Any,
            "self reference object content",
        );

        Ok(manifest)
    }

    /// Try replacing an object, returning null on error instead of throwing.
    #[napi]
    pub fn try_replace(
        &self,
        original_oid: String,
        new_content: Buffer,
        path: Option<String>,
    ) -> Option<ReplaceManifest> {
        self.replace(original_oid, new_content, path).ok()
    }

    /// Read an individual replacement revision node by object id.
    #[napi]
    pub fn get_replace_node(&self, oid: String) -> Result<Option<Replace>> {
        let r = thread_local(&self.inner);
        let node_ref = format!("refs/replace/{oid}");
        let reference = match r.find_reference(&node_ref) {
            Ok(ref_) => ref_,
            Err(_) => return Ok(None),
        };
        let target_id = reference.target().id().to_owned();
        let blob = match r.find_blob(target_id) {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };
        match serde_json::from_slice::<Replace>(&blob.data) {
            Ok(node) => Ok(Some(node)),
            Err(_) => Ok(None),
        }
    }
}
