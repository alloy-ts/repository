use std::env;
use std::fs;

use napi::bindgen_prelude::{Buffer, Either};
use napi::Result;
use napi_derive::napi;
use serde::{Deserialize, Serialize};

use crate::error::{err_str, thread_local};

/// Revision history entry inside a ReplaceManifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct ReplaceNode {
    /// Revision commit / content SHA.
    pub id: String,
    /// Parent revision SHA, if any.
    pub parent: Option<String>,
    /// Creation timestamp in milliseconds.
    pub created_at: f64,
    /// Author name.
    pub author_name: Option<String>,
    /// Author email.
    pub author_email: Option<String>,
}

/// Revision history manifest for refs/replace/{oid} or refs/replace/{entryPath}.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct ReplaceManifest {
    /// Initial object SHA reference.
    pub id: String,
    /// Optional path if the object corresponds to a file entry in the current working tree.
    pub path: Option<String>,
    /// Object type ("blob", "commit", "tree", "tag").
    pub object_type: String,
    /// Head revision SHA (points to the latest revision node).
    pub head: String,
    /// Historic revision list.
    pub versions: Vec<ReplaceNode>,
    /// Total number of revisions.
    pub size: u32,
    /// Last updated timestamp in milliseconds.
    pub updated_at: f64,
}

/// Options for replacing an object.
#[derive(Debug, Clone, Default)]
#[napi(object)]
pub struct ReplaceOptions {
    /// Force replacement even if replace reference exists or types mismatch (-f).
    pub force: Option<bool>,
    /// Optional relative path of the file entry in the current working tree.
    pub entry_path: Option<String>,
    /// Author name for the revision record.
    pub author_name: Option<String>,
    /// Author email for the revision record.
    pub author_email: Option<String>,
}

/// Check if replace refs are enabled in config or env.
pub fn is_replace_refs_enabled(repo: &gix::Repository) -> bool {
    if let Ok(val) = env::var("GIT_NO_REPLACE_OBJECTS") {
        let lower = val.trim().to_lowercase();
        if lower == "1" || lower == "true" || lower == "yes" {
            return false;
        }
    }
    repo.config_snapshot()
        .boolean("core.useReplaceRefs")
        .unwrap_or(true)
}

/// Write loose object directly in .git/objects/xx/yyyy...
fn write_loose_object(repo: &gix::Repository, oid_str: &str, kind: gix::object::Kind, content: &[u8]) -> Result<()> {
    if oid_str.len() < 4 {
        return Err(err_str("write_loose_object", "invalid object ID length"));
    }
    let git_dir = repo.git_dir();
    let (dir_prefix, file_suffix) = oid_str.split_at(2);
    let obj_dir = git_dir.join("objects").join(dir_prefix);
    fs::create_dir_all(&obj_dir).map_err(|e| err_str("write_loose_object", e.to_string()))?;

    let obj_path = obj_dir.join(file_suffix);

    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    let kind_str = match kind {
        gix::object::Kind::Blob => "blob",
        gix::object::Kind::Commit => "commit",
        gix::object::Kind::Tree => "tree",
        gix::object::Kind::Tag => "tag",
    };

    let header = format!("{} {}\0", kind_str, content.len());
    let mut payload = Vec::with_capacity(header.len() + content.len());
    payload.extend_from_slice(header.as_bytes());
    payload.extend_from_slice(content);

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(&payload)
        .map_err(|e| err_str("write_loose_object", e.to_string()))?;
    let compressed = encoder
        .finish()
        .map_err(|e| err_str("write_loose_object", e.to_string()))?;

    fs::write(&obj_path, compressed).map_err(|e| err_str("write_loose_object", e.to_string()))?;
    Ok(())
}

/// Create or update replacement refs under refs/replace/ and refs/objects/.
#[napi]
pub fn replace(
    repo_handle: &crate::repository::Repository,
    old_oid_str: String,
    new_content: Either<Buffer, String>,
    options: Option<ReplaceOptions>,
) -> Result<String> {
    let repo = thread_local(&repo_handle.inner);
    let opts = options.unwrap_or_default();
    let force = opts.force.unwrap_or(false);

    let content_bytes = match new_content {
        Either::A(b) => b.as_ref().to_vec(),
        Either::B(s) => s.as_bytes().to_vec(),
    };

    // Validate object type and existence
    let old_oid = gix::ObjectId::from_hex(old_oid_str.as_bytes())
        .map_err(|e| err_str("replace", format!("invalid SHA: {e}")))?;

    let old_obj = repo
        .find_object(old_oid)
        .map_err(|e| err_str("replace", format!("target object not found: {e}")))?;

    let obj_kind_str = match old_obj.kind {
        gix::object::Kind::Blob => "blob",
        gix::object::Kind::Commit => "commit",
        gix::object::Kind::Tree => "tree",
        gix::object::Kind::Tag => "tag",
    };

    // 1. Check core.useReplaceRefs
    if !is_replace_refs_enabled(&repo) {
        // Direct in-place replacement
        write_loose_object(&repo, &old_oid_str, old_obj.kind, &content_bytes)?;
        return Ok(old_oid_str);
    }

    // Replacement object type is blob when passing raw bytes/string unless force is used
    if old_obj.kind != gix::object::Kind::Blob && !force {
        return Err(err_str(
            "replace",
            format!("replaced object ({obj_kind_str}) and replacement object (blob) must be of the same type; use force (-f) to bypass"),
        ));
    }

    // 3. Write new replacement object according to target object type or blob if forced
    let new_oid = repo
        .write_blob(&content_bytes)
        .map_err(|e| err_str("replace", format!("failed to write replacement blob: {e}")))?;
    let new_oid_str = new_oid.to_string();

    // 4. Check if replace ref already exists unless force is set
    let replace_ref_name = format!("refs/replace/{old_oid_str}");
    if !force && repo.try_find_reference(&replace_ref_name).map_err(|e| err_str("replace", e.to_string()))?.is_some() {
        return Err(err_str(
            "replace",
            format!("replace reference '{replace_ref_name}' already exists; use force to overwrite"),
        ));
    }

    // 5. Build/Update ReplaceManifest
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0);

    let mut manifest = match read_replace_manifest(&repo, &old_oid_str)? {
        Some(mut existing) => {
            let parent_id = existing.head.clone();
            let node = ReplaceNode {
                id: new_oid_str.clone(),
                parent: Some(parent_id),
                created_at: now,
                author_name: opts.author_name.clone(),
                author_email: opts.author_email.clone(),
            };
            existing.head = new_oid_str.clone();
            existing.versions.push(node);
            existing.size = existing.versions.len() as u32;
            existing.updated_at = now;
            if opts.entry_path.is_some() {
                existing.path = opts.entry_path.clone();
            }
            existing
        }
        None => {
            let node = ReplaceNode {
                id: new_oid_str.clone(),
                parent: None,
                created_at: now,
                author_name: opts.author_name.clone(),
                author_email: opts.author_email.clone(),
            };
            ReplaceManifest {
                id: old_oid_str.clone(),
                path: opts.entry_path.clone(),
                object_type: obj_kind_str.to_string(),
                head: new_oid_str.clone(),
                versions: vec![node],
                size: 1,
                updated_at: now,
            }
        }
    };

    let manifest_json = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| err_str("replace", format!("failed to serialize manifest: {e}")))?;

    // 6. Write manifest ref at refs/replace/{oldOid}
    let manifest_blob_id = repo
        .write_blob(&manifest_json)
        .map_err(|e| err_str("replace", format!("failed to write manifest blob: {e}")))?;

    repo.reference(
        replace_ref_name.as_str(),
        manifest_blob_id,
        gix::refs::transaction::PreviousValue::Any,
        "replace: update replace ref",
    )
    .map_err(|e| err_str("replace", format!("failed to update replace reference: {e}")))?;

    // 7. Write entry path reference if provided or in current working tree
    let entry_path = opts.entry_path.or_else(|| check_entry_in_index(&repo, &old_oid));
    if let Some(path_str) = entry_path {
        manifest.path = Some(path_str.clone());
        let path_ref_name = format!("refs/replace/{path_str}");
        let _ = repo.reference(
            path_ref_name.as_str(),
            manifest_blob_id,
            gix::refs::transaction::PreviousValue::Any,
            "replace: update path replace ref",
        );
    }

    // 8. Mirror object reference at refs/objects/{oldOid} pointing to refs/objects/{newOid}
    let obj_ref_name = format!("refs/objects/{old_oid_str}");
    let _ = repo.reference(
        obj_ref_name.as_str(),
        new_oid,
        gix::refs::transaction::PreviousValue::Any,
        "replace: mirror object ref",
    );

    let new_obj_ref_name = format!("refs/objects/{new_oid_str}");
    let _ = repo.reference(
        new_obj_ref_name.as_str(),
        new_oid,
        gix::refs::transaction::PreviousValue::Any,
        "replace: new object ref",
    );

    Ok(new_oid_str)
}

/// Attempt replacing an object; falls back to in-place loose object write when useReplaceRefs is false.
#[napi]
pub fn try_replace(
    repo_handle: &crate::repository::Repository,
    old_oid_str: String,
    new_content: Either<Buffer, String>,
    options: Option<ReplaceOptions>,
) -> Result<String> {
    replace(repo_handle, old_oid_str, new_content, options)
}

/// Helper to read replace manifest from refs/replace/{key}.
fn read_replace_manifest(repo: &gix::Repository, key: &str) -> Result<Option<ReplaceManifest>> {
    let ref_name = if key.starts_with("refs/") {
        key.to_string()
    } else {
        format!("refs/replace/{key}")
    };

    let reference = match repo.try_find_reference(&ref_name) {
        Ok(Some(rf)) => rf,
        _ => return Ok(None),
    };

    let target_id = match reference.try_id() {
        Some(id) => id.detach(),
        None => return Ok(None),
    };

    let blob = match repo.find_blob(target_id) {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };

    let manifest: ReplaceManifest = match serde_json::from_slice(&blob.data) {
        Ok(m) => m,
        Err(_) => return Ok(None),
    };

    Ok(Some(manifest))
}

/// Check if object OID exists in current repository index.
fn check_entry_in_index(repo: &gix::Repository, oid: &gix::ObjectId) -> Option<String> {
    let index = repo.index().ok()?;
    for entry in index.entries() {
        if entry.id == *oid {
            return Some(entry.path(&index).to_string());
        }
    }
    None
}

/// Check if core.useReplaceRefs is enabled for the repository.
#[napi]
pub fn use_replace_refs(repo_handle: &crate::repository::Repository) -> bool {
    let repo = thread_local(&repo_handle.inner);
    is_replace_refs_enabled(&repo)
}

/// Retrieve the ReplaceManifest for a given reference SHA or entry path.
#[napi]
pub fn get_replace_manifest(
    repo_handle: &crate::repository::Repository,
    key: String,
) -> Result<Option<ReplaceManifest>> {
    let repo = thread_local(&repo_handle.inner);
    read_replace_manifest(&repo, &key)
}

/// Retrieve a specific ReplaceNode / revision by revision SHA from a replace manifest.
#[napi]
pub fn get_replace_node(
    repo_handle: &crate::repository::Repository,
    key: String,
    revision_id: String,
) -> Result<Option<ReplaceNode>> {
    if let Some(manifest) = get_replace_manifest(repo_handle, key)? {
        for v in manifest.versions {
            if v.id == revision_id {
                return Ok(Some(v));
            }
        }
    }
    Ok(None)
}

/// Alias for get_replace_node.
#[napi]
pub fn get_revision(
    repo_handle: &crate::repository::Repository,
    key: String,
    revision_id: String,
) -> Result<Option<ReplaceNode>> {
    get_replace_node(repo_handle, key, revision_id)
}

/// Get the underlying replacement reference string.
#[napi]
pub fn get_replace_ref(
    repo_handle: &crate::repository::Repository,
    key: String,
) -> Result<Option<String>> {
    let repo = thread_local(&repo_handle.inner);
    let ref_name = format!("refs/replace/{key}");
    match repo.try_find_reference(&ref_name) {
        Ok(Some(rf)) => Ok(rf.try_id().map(|id| id.to_string())),
        _ => Ok(None),
    }
}

/// Resolve/dereference a given object ID to its latest replacement content ID if replaced.
#[napi]
pub fn deref_replace(
    repo_handle: &crate::repository::Repository,
    oid_str: String,
) -> Result<String> {
    let repo = thread_local(&repo_handle.inner);
    if !is_replace_refs_enabled(&repo) {
        return Ok(oid_str);
    }

    if let Some(manifest) = read_replace_manifest(&repo, &oid_str)? {
        return Ok(manifest.head);
    }

    Ok(oid_str)
}

/// Internal helper to resolve replace ID iteratively up to depth 32.
pub fn resolve_replace_id(repo: &gix::Repository, initial_oid: gix::ObjectId) -> gix::ObjectId {
    if !is_replace_refs_enabled(repo) {
        return initial_oid;
    }

    let mut current_str = initial_oid.to_string();
    let mut depth = 0;

    while depth < 32 {
        if let Ok(Some(manifest)) = read_replace_manifest(repo, &current_str) {
            if manifest.head == current_str {
                break;
            }
            current_str = manifest.head;
            depth += 1;
        } else {
            break;
        }
    }

    gix::ObjectId::from_hex(current_str.as_bytes()).unwrap_or(initial_oid)
}
