use std::sync::Arc;

use gix::objs::Write;
use napi::bindgen_prelude::{Buffer, Either};
use napi::Result;
use napi_derive::napi;
use serde::{Deserialize, Serialize};

use crate::error::{err, err_str, parse_oid, thread_local};
use crate::repository::Repository;

/// An individual Replace revision node.
#[napi(object)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Replace {
    /// Reference / OID SHA of this specific replacement object node.
    pub this: String,
    /// Reference / OID SHA of the previous replacement node, or null if first.
    pub prev: Option<String>,
    /// Reference / OID SHA of the next replacement node, or null if latest.
    pub next: Option<String>,
    /// Reference / OID SHA of the latest head replacement object.
    pub head: Option<String>,
    /// Name of the author who performed or originated this replacement.
    #[serde(rename = "authorName")]
    #[napi(js_name = "authorName")]
    pub author_name: String,
    /// Email of the author who performed or originated this replacement.
    #[serde(rename = "authorEmail")]
    #[napi(js_name = "authorEmail")]
    pub author_email: String,
    /// Name of the committer associated with this replacement commit/entry.
    #[serde(rename = "committerName", skip_serializing_if = "Option::is_none")]
    #[napi(js_name = "committerName")]
    pub committer_name: Option<String>,
    /// Email of the committer associated with this replacement commit/entry.
    #[serde(rename = "committerEmail", skip_serializing_if = "Option::is_none")]
    #[napi(js_name = "committerEmail")]
    pub committer_email: Option<String>,
    /// ISO timestamp string when this replacement was created or committed.
    #[serde(rename = "updated_at")]
    #[napi(js_name = "updated_at")]
    pub updated_at: String,
}

/// A Revision node (exported for TypeScript compatibility alongside [`Replace`]).
#[napi(object)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Revision {
    /// Reference / OID SHA of this specific replacement object node.
    pub this: String,
    /// Reference / OID SHA of the previous replacement node, or null if first.
    pub prev: Option<String>,
    /// Reference / OID SHA of the next replacement node, or null if latest.
    pub next: Option<String>,
    /// Reference / OID SHA of the latest head replacement object.
    pub head: Option<String>,
    /// Name of the author who performed or originated this replacement.
    #[serde(rename = "authorName")]
    #[napi(js_name = "authorName")]
    pub author_name: String,
    /// Email of the author who performed or originated this replacement.
    #[serde(rename = "authorEmail")]
    #[napi(js_name = "authorEmail")]
    pub author_email: String,
    /// Name of the committer associated with this replacement commit/entry.
    #[serde(rename = "committerName", skip_serializing_if = "Option::is_none")]
    #[napi(js_name = "committerName")]
    pub committer_name: Option<String>,
    /// Email of the committer associated with this replacement commit/entry.
    #[serde(rename = "committerEmail", skip_serializing_if = "Option::is_none")]
    #[napi(js_name = "committerEmail")]
    pub committer_email: Option<String>,
    /// ISO timestamp string when this replacement was created or committed.
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
            committer_name: r.committer_name,
            committer_email: r.committer_email,
            updated_at: r.updated_at,
        }
    }
}

/// The root replacement manifest object for a stable static artifact/entry/object history.
#[napi(object)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplaceManifest {
    /// Stable artifact ID ("aid", the initial object OID).
    pub aid: String,
    /// Stable initial object ID (alias for aid).
    pub id: String,
    /// `$ref` reference string pointing to `refs/replace/{aid}`.
    #[serde(rename = "$ref", skip_serializing_if = "Option::is_none")]
    #[napi(js_name = "$ref")]
    pub ref_path: Option<String>,
    /// Reference / OID SHA of the latest replacement object ("head").
    pub head: String,
    /// Object type: `"reference" | "blob" | "tag" | "commit" | "tree" | "entry"`.
    #[serde(rename = "type")]
    #[napi(js_name = "type")]
    pub type_name: String,
    /// File entry path if the replaced object is committed in current working tree / branch.
    pub path: Option<String>,
    /// Flat array of Replace metadata nodes.
    pub revisions: Vec<Replace>,
    /// Flat array of Replace metadata nodes (alias for revisions).
    pub versions: Vec<Replace>,
}

#[napi(object)]
#[derive(Clone, Debug, Default)]
pub struct ReplaceOptions {
    /// Bypass type-matching and pre-existing replace reference restrictions.
    pub force: Option<bool>,
    /// Author/committer name recorded in metadata.
    pub author_name: Option<String>,
    /// Author/committer email recorded in metadata.
    pub author_email: Option<String>,
    /// Explicit target object kind when writing replacement ("blob", "commit", "tree", "tag").
    pub kind: Option<String>,
}

impl ReplaceManifest {
    /// Return the number of replace revisions in the history.
    pub fn size(&self) -> usize {
        self.revisions.len()
    }
}

/// Helper to check if replacement references are enabled in repository configuration or environment.
pub fn is_replace_refs_enabled(repo: &Arc<gix::ThreadSafeRepository>) -> bool {
    if let Ok(val) = std::env::var("GIT_NO_REPLACE_OBJECTS") {
        if val == "1" || val.eq_ignore_ascii_case("true") {
            return false;
        }
    }
    let r = thread_local(repo);
    let config = r.config_snapshot();
    if let Some(val) = config
        .string("core.useReplaceRefs")
        .or_else(|| config.string("core.usereplacerefs"))
    {
        let val_str = String::from_utf8_lossy(&val);
        if val_str.eq_ignore_ascii_case("false") || val_str == "0" {
            return false;
        }
    }
    if let Some(b) = config
        .boolean("core.useReplaceRefs")
        .or_else(|| config.boolean("core.usereplacerefs"))
    {
        return b;
    }
    true
}

/// Resolve an OID through any replacement refs under `refs/replace/*` if enabled.
pub fn resolve_replace_oid(
    repo: &Arc<gix::ThreadSafeRepository>,
    oid: gix::ObjectId,
) -> gix::ObjectId {
    if !is_replace_refs_enabled(repo) {
        return oid;
    }

    let r = thread_local(repo);
    let mut current = oid;
    let mut depth = 0;
    while depth < 32 {
        let ref_name = format!("refs/replace/{}", current);
        let rf = match r.find_reference(&ref_name) {
            Ok(rf) => rf,
            Err(_) => break,
        };

        let target_id = match rf.target().try_id() {
            Some(id) => id.to_owned(),
            None => break,
        };

        let obj = match r.find_object(target_id) {
            Ok(o) => o,
            Err(_) => break,
        };

        let next_oid_str = if let Ok(manifest) = serde_json::from_slice::<ReplaceManifest>(&obj.data) {
            manifest.head
        } else if let Ok(node) = serde_json::from_slice::<Replace>(&obj.data) {
            node.head.unwrap_or(node.this)
        } else {
            break;
        };

        match parse_oid(&next_oid_str) {
            Ok(next_oid) => {
                if next_oid == current {
                    break;
                }
                current = next_oid;
                depth += 1;
            }
            Err(_) => break,
        }
    }

    current
}

/// Helper to find a file path in the current HEAD or default branch tree corresponding to an OID.
fn find_entry_path_in_head(
    repo: &Arc<gix::ThreadSafeRepository>,
    oid: &gix::ObjectId,
) -> Option<String> {
    let r = thread_local(repo);
    let head_commit = r.head_commit().ok()?;
    let tree = head_commit.tree().ok()?;

    let mut recorder = gix::traverse::tree::Recorder::default();
    let _ = tree.traverse().breadthfirst(&mut recorder);
    for entry in recorder.records {
        if entry.oid == *oid {
            return Some(entry.filepath.to_string());
        }
    }
    None
}

/// Helper to extract author/committer signature metadata from target object or HEAD commit.
fn extract_commit_metadata(
    repo: &Arc<gix::ThreadSafeRepository>,
    target_oid: &gix::ObjectId,
    options: Option<&ReplaceOptions>,
) -> (String, String, Option<String>, Option<String>, String) {
    let r = thread_local(repo);

    let mut author_name = options.and_then(|o| o.author_name.clone());
    let mut author_email = options.and_then(|o| o.author_email.clone());
    let mut committer_name = None;
    let mut committer_email = None;
    let mut timestamp = None;

    // Try reading target commit object directly
    if let Ok(commit) = r.find_commit(*target_oid) {
        if let Ok(sig) = commit.author() {
            if author_name.is_none() {
                author_name = Some(sig.name.to_string());
            }
            if author_email.is_none() {
                author_email = Some(sig.email.to_string());
            }
            timestamp = sig.time().ok().and_then(|t| {
                t.format(gix::date::time::format::ISO8601).ok()
            });
        }
        if let Ok(sig) = commit.committer() {
            committer_name = Some(sig.name.to_string());
            committer_email = Some(sig.email.to_string());
        }
    } else if let Ok(head_commit) = r.head_commit() {
        // Fall back to HEAD commit signature if target is a blob/tree
        if let Ok(sig) = head_commit.author() {
            if author_name.is_none() {
                author_name = Some(sig.name.to_string());
            }
            if author_email.is_none() {
                author_email = Some(sig.email.to_string());
            }
            if timestamp.is_none() {
                timestamp = sig.time().ok().and_then(|t| {
                    t.format(gix::date::time::format::ISO8601).ok()
                });
            }
        }
        if let Ok(sig) = head_commit.committer() {
            committer_name = Some(sig.name.to_string());
            committer_email = Some(sig.email.to_string());
        }
    }

    let default_sig = r.committer().and_then(|res| res.ok());
    let final_author_name = author_name
        .or_else(|| default_sig.as_ref().map(|s| s.name.to_string()))
        .unwrap_or_else(|| "Git Replace".to_string());
    let final_author_email = author_email
        .or_else(|| default_sig.as_ref().map(|s| s.email.to_string()))
        .unwrap_or_else(|| "replace@git.local".to_string());

    let final_timestamp = timestamp.unwrap_or_else(|| {
        gix::date::Time::now_local_or_utc()
            .format(gix::date::time::format::ISO8601)
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
    });

    (
        final_author_name,
        final_author_email,
        committer_name,
        committer_email,
        final_timestamp,
    )
}

#[napi]
impl Repository {
    /// Replace an existing object or entry with new content.
    ///
    /// When `core.useReplaceRefs` config is `false` or `GIT_NO_REPLACE_OBJECTS` is set,
    /// replaces content directly in place and returns the passed target string.
    ///
    /// When replacement references are enabled (default):
    /// Creates the new replacement object in the object store, creates a single revision
    /// node at `refs/replace/{newOid}`, and updates the canonical history manifest at
    /// `refs/replace/{initOid}` (and `refs/replace/{entryPath}` if matched).
    #[napi]
    pub fn replace(
        &self,
        target: String,
        new_content: Either<Buffer, String>,
        options: Option<ReplaceOptions>,
    ) -> Result<String> {
        let force = options.as_ref().and_then(|o| o.force).unwrap_or(false);

        let raw_bytes = match &new_content {
            Either::A(buf) => buf.as_ref().to_vec(),
            Either::B(str_val) => str_val.as_bytes().to_vec(),
        };

        // If core.useReplaceRefs is disabled, replace content directly in place.
        if !is_replace_refs_enabled(&self.inner) {
            let r = thread_local(&self.inner);
            let _ = r
                .write_blob(raw_bytes)
                .map_err(|e| err("replace_direct", e))?;
            return Ok(target);
        }

        let r = thread_local(&self.inner);

        // Resolve target object or reference
        let (init_oid, old_kind, entry_path) = match parse_oid(&target) {
            Ok(oid) => {
                let obj = r.find_object(oid).map_err(|e| err("find_object", e))?;
                let kind_str = match obj.kind {
                    gix::object::Kind::Commit => "commit",
                    gix::object::Kind::Tree => "tree",
                    gix::object::Kind::Blob => "blob",
                    gix::object::Kind::Tag => "tag",
                }
                .to_string();
                let path = find_entry_path_in_head(&self.inner, &oid);
                (oid, kind_str, path)
            }
            Err(_) => {
                // Try finding target as reference or entry path
                if let Ok(rf) = r.find_reference(&target) {
                    let oid = rf
                        .target()
                        .try_id()
                        .ok_or_else(|| err_str("replace", "reference does not point to object"))?
                        .to_owned();
                    let obj = r.find_object(oid).map_err(|e| err("find_object", e))?;
                    let kind_str = match obj.kind {
                        gix::object::Kind::Commit => "commit",
                        gix::object::Kind::Tree => "tree",
                        gix::object::Kind::Blob => "blob",
                        gix::object::Kind::Tag => "tag",
                    }
                    .to_string();
                    (oid, kind_str, None)
                } else {
                    // Try looking up target as a file path in HEAD tree
                    let head_commit = r
                        .head_commit()
                        .map_err(|e| err_str("replace", format!("cannot resolve target '{target}': {e}")))?;
                    let tree = head_commit
                        .tree()
                        .map_err(|e| err_str("replace", format!("cannot resolve tree: {e}")))?;
                    let entry = tree
                        .lookup_entry_by_path(&target)
                        .map_err(|e| err_str("replace", format!("cannot resolve path '{target}': {e}")))?
                        .ok_or_else(|| err_str("replace", format!("target path '{target}' not found in HEAD tree")))?;
                    let oid = entry.object_id();
                    let obj = r.find_object(oid).map_err(|e| err("find_object", e))?;
                    let kind_str = match obj.kind {
                        gix::object::Kind::Commit => "commit",
                        gix::object::Kind::Tree => "tree",
                        gix::object::Kind::Blob => "blob",
                        gix::object::Kind::Tag => "tag",
                    }
                    .to_string();
                    (oid, kind_str, Some(target.clone()))
                }
            }
        };

        let init_oid_str = init_oid.to_string();
        let main_ref_name = format!("refs/replace/{init_oid_str}");

        // Unless force is given, verify that replace reference does not yet exist
        if !force && r.find_reference(&main_ref_name).is_ok() {
            return Err(err_str(
                "replace",
                format!("replace reference '{main_ref_name}' already exists; use force (-f) to overwrite"),
            ));
        }

        // Determine target object kind for replacement object
        let explicit_kind = options.as_ref().and_then(|o| o.kind.as_deref());
        let new_obj_kind = match explicit_kind {
            Some("commit") => gix::object::Kind::Commit,
            Some("tree") => gix::object::Kind::Tree,
            Some("tag") => gix::object::Kind::Tag,
            Some("blob") | Some("entry") => gix::object::Kind::Blob,
            _ => match old_kind.as_str() {
                "commit" => gix::object::Kind::Commit,
                "tree" => gix::object::Kind::Tree,
                "tag" => gix::object::Kind::Tag,
                _ => gix::object::Kind::Blob,
            },
        };

        // Write replacement object to object store
        let new_oid = r
            .write_stream(new_obj_kind, raw_bytes.len() as u64, &mut raw_bytes.as_slice())
            .map_err(|e| err("write_replacement_object", e))?;
        let new_oid_str = new_oid.to_string();

        // Unless force is given, ensure old and replacement objects are of the same type
        if !force {
            let new_obj = r.find_object(new_oid).map_err(|e| err("find_new_object", e))?;
            let new_kind_str = match new_obj.kind {
                gix::object::Kind::Commit => "commit",
                gix::object::Kind::Tree => "tree",
                gix::object::Kind::Blob => "blob",
                gix::object::Kind::Tag => "tag",
            };

            let expected_kind = if old_kind == "entry" { "blob" } else { &old_kind };
            if expected_kind != new_kind_str {
                return Err(err_str(
                    "replace",
                    format!(
                        "replaced object ({old_kind}) and replacement object ({new_kind_str}) must be of the same type; use force (-f) to bypass"
                    ),
                ));
            }
        }

        // Extract metadata signatures
        let (author_name, author_email, committer_name, committer_email, updated_at) =
            extract_commit_metadata(&self.inner, &init_oid, options.as_ref());

        // Read existing manifest if present at refs/replace/{init_oid}
        let existing_manifest = self.get_replace_manifest(init_oid_str.clone()).ok().flatten();

        let prev_sha = existing_manifest.as_ref().map(|m| m.head.clone());

        let new_node = Replace {
            this: new_oid_str.clone(),
            prev: prev_sha.clone(),
            next: None,
            head: Some(new_oid_str.clone()),
            author_name,
            author_email,
            committer_name,
            committer_email,
            updated_at,
        };

        // Update revisions list
        let mut revisions = existing_manifest
            .as_ref()
            .map(|m| m.revisions.clone())
            .unwrap_or_default();

        // Update previous node's `next` pointer
        if let Some(last) = revisions.last_mut() {
            last.next = Some(new_oid_str.clone());
        }

        // Set `head` on all revisions to new_oid_str
        for rev in &mut revisions {
            rev.head = Some(new_oid_str.clone());
        }

        revisions.push(new_node.clone());

        let manifest_type = if entry_path.is_some() {
            "entry".to_string()
        } else {
            old_kind
        };

        let manifest = ReplaceManifest {
            aid: init_oid_str.clone(),
            id: init_oid_str.clone(),
            ref_path: Some(format!("refs/replace/{init_oid_str}")),
            head: new_oid_str.clone(),
            type_name: manifest_type,
            path: entry_path.clone(),
            revisions: revisions.clone(),
            versions: revisions.clone(),
        };

        // Write individual Replace node JSON blob
        let rev_json = serde_json::to_vec(&new_node).map_err(|e| err("serialize_revision", e))?;
        let rev_blob_id = r
            .write_blob(rev_json)
            .map_err(|e| err("write_revision_blob", e))?;

        // Create/update ref at refs/replace/{newOid}
        let node_ref_name = format!("refs/replace/{new_oid_str}");
        r.reference(
            node_ref_name,
            rev_blob_id,
            gix::refs::transaction::PreviousValue::Any,
            "create replace revision node",
        )
        .map_err(|e| err("create_replace_node_ref", e))?;

        // Write ReplaceManifest JSON blob
        let manifest_json =
            serde_json::to_vec(&manifest).map_err(|e| err("serialize_manifest", e))?;
        let manifest_blob_id = r
            .write_blob(manifest_json)
            .map_err(|e| err("write_manifest_blob", e))?;

        // Create/update canonical ref at refs/replace/{initOid}
        r.reference(
            main_ref_name,
            manifest_blob_id,
            gix::refs::transaction::PreviousValue::Any,
            "update replace manifest",
        )
        .map_err(|e| err("create_replace_manifest_ref", e))?;

        // If entry_path exists, also create/update ref at refs/replace/{entryPath}
        if let Some(ref path) = entry_path {
            let path_ref_name = format!("refs/replace/{path}");
            let _ = r.reference(
                path_ref_name,
                manifest_blob_id,
                gix::refs::transaction::PreviousValue::Any,
                "update replace entry path manifest",
            );
        }

        Ok(new_oid_str)
    }

    /// Like `replace`, but returns `null` if replacement fails rather than throwing.
    #[napi]
    pub fn try_replace(
        &self,
        target: String,
        new_content: Either<Buffer, String>,
        options: Option<ReplaceOptions>,
    ) -> Result<Option<String>> {
        match self.replace(target, new_content, options) {
            Ok(res) => Ok(Some(res)),
            Err(_) => Ok(None),
        }
    }

    /// Get the [`ReplaceManifest`] stored at `refs/replace/{target}` (where target is oid or entry path).
    #[napi]
    pub fn get_replace_manifest(&self, target: String) -> Result<Option<ReplaceManifest>> {
        let r = thread_local(&self.inner);
        let ref_name = if target.starts_with("refs/replace/") {
            target
        } else {
            format!("refs/replace/{target}")
        };

        let rf = match r.find_reference(&ref_name) {
            Ok(rf) => rf,
            Err(_) => return Ok(None),
        };

        let oid = match rf.target().try_id() {
            Some(id) => id.to_owned(),
            None => return Ok(None),
        };

        let obj = match r.find_object(oid) {
            Ok(o) => o,
            Err(_) => return Ok(None),
        };

        let manifest: ReplaceManifest = match serde_json::from_slice(&obj.data) {
            Ok(m) => m,
            Err(_) => return Ok(None),
        };

        Ok(Some(manifest))
    }

    /// Get an individual [`Replace`] revision node stored at `refs/replace/{oid}`.
    #[napi]
    pub fn get_replace_revision(&self, target: String) -> Result<Option<Replace>> {
        let r = thread_local(&self.inner);
        let ref_name = if target.starts_with("refs/replace/") {
            target
        } else {
            format!("refs/replace/{target}")
        };

        let rf = match r.find_reference(&ref_name) {
            Ok(rf) => rf,
            Err(_) => return Ok(None),
        };

        let oid = match rf.target().try_id() {
            Some(id) => id.to_owned(),
            None => return Ok(None),
        };

        let obj = match r.find_object(oid) {
            Ok(o) => o,
            Err(_) => return Ok(None),
        };

        let node: Replace = match serde_json::from_slice(&obj.data) {
            Ok(r) => r,
            Err(_) => return Ok(None),
        };

        Ok(Some(node))
    }
}
