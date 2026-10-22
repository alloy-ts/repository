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
        let _orig_id = parse_oid(&original_oid)?;

        // 1. Write the new object content
        let new_oid = r
            .write_blob(new_content.to_vec())
            .map_err(|e| err("write_blob", e))?
            .detach();
        let new_oid_str = new_oid.to_string();

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
                        r#type: "blob".to_string(),
                        head: original_oid.clone(),
                        revisions: vec![],
                    })
            }
            Err(_) => ReplaceManifest {
                id: original_oid.clone(),
                path: path.clone(),
                r#type: if path.is_some() { "entry" } else { "blob" }.to_string(),
                head: original_oid.clone(),
                revisions: vec![],
            },
        };

        let prev_head = if manifest.revisions.is_empty() {
            None
        } else {
            Some(manifest.head.clone())
        };

        // 3. Construct the new individual revision node
        let author = r.author().and_then(|res| res.ok());
        let author_name = author
            .as_ref()
            .map(|a| String::from_utf8_lossy(a.name).into_owned())
            .unwrap_or_else(|| "gitoxide".to_string());
        let author_email = author
            .as_ref()
            .map(|a| String::from_utf8_lossy(a.email).into_owned())
            .unwrap_or_else(|| "gitoxide@localhost".to_string());

        let node = Replace {
            this_id: new_oid_str.clone(),
            prev: prev_head.clone(),
            next: None,
            head: Some(new_oid_str.clone()),
            author_name,
            author_email,
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
