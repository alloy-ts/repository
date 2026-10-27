use std::sync::Arc;

use crate::error::RepositoryResult as Result;
use napi::bindgen_prelude::{Buffer, Either};
use napi_derive::napi;
use serde::{Deserialize, Serialize};

use crate::error::{err, err_str, thread_local};
use crate::object::kind_to_string;
use crate::repository::Repository;

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Options controlling [`Repository::replace`] / [`Repository::try_replace`].
#[napi(object)]
pub struct ReplaceOptions {
    /// Bypass the "replace reference must not already exist" rule (`-f`). The
    /// same-type restriction is always applied at the manifest level (the
    /// original object's kind is preserved in the revision history), so this flag
    /// mainly allows overwriting an existing `refs/replace/*` entry.
    pub force: Option<bool>,
    /// Explicitly enable or disable the replace-ref machinery. When omitted the
    /// `core.useReplaceRefs` config is consulted (git's default is `true`). When
    /// `false`, the new content is written as a blob and returned directly, and no
    /// `refs/replace/*` history is recorded.
    pub use_replace_refs: Option<bool>,
    /// Log message attached to the reference transaction.
    pub message: Option<String>,
}

/// A single revision node in a replace history.
///
/// Each revision is stored individually at `refs/replace/<contentOid>` (the blob
/// there is exactly this JSON), and the same object is also collected into the
/// canonical manifest's `revisions` array. The node is self-describing: `this`
/// is the revision's own content oid, `prev`/`next` are the surrounding
/// revision content oids (when known), and `head` caches the latest revision
/// content oid for fast traversal without walking the chain.
#[derive(Clone, Serialize, Deserialize)]
#[napi(object, js_name = "Revision")]
pub struct Replace {
    /// This revision's own content oid (its reference).
    pub r#this: String,
    /// The content oid of the previous revision, if any.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub prev: Option<String>,
    /// The content oid of the next revision, if any.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub next: Option<String>,
    /// The content oid of the latest revision (cached for fast traversal).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub head: Option<String>,
    /// The name of the identity that created this revision.
    pub r#authorName: String,
    /// The email of the identity that created this revision.
    pub r#authorEmail: String,
    /// ISO-8601 timestamp of when this revision was created.
    pub r#updated_at: String,
    /// Name of the committer of the commit that contains this entry (entry-type replaces).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub r#committerName: Option<String>,
    /// Email of the committer of the commit that contains this entry (entry-type replaces).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub r#committerEmail: Option<String>,
    /// ISO-8601 timestamp of the commit that contains this entry (entry-type replaces).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub r#commitTimestamp: Option<String>,
}

/// The revision-history manifest for a single replaced object, as stored in the
/// blob pointed at by `refs/replace/<oid|path>`.
///
/// `id`/`$ref` identify the original object, `head` is the latest revision
/// content id, `revisions` is the full ordered list of [`ReplaceNode`]s (oldest
/// first), and `path` is present only when the replaced object is a tracked tree
/// entry (`type == "entry"`).
#[napi(object)]
pub struct ReplaceManifest {
    /// The original object id for the entire revision-history tree.
    pub id: String,
    /// Entry path in the working tree, present only when `type == "entry"`.
    pub path: Option<String>,
    /// The object kind (`entry` / `blob` / `tree` / `commit` / `tag`).
    pub r#type: String,
    /// The object id of the latest revision content.
    pub head: String,
    /// All revision nodes, oldest first; the last element's `this` equals `head`.
    pub revisions: Vec<Replace>,
}

/// The outcome of a [`Repository::replace`] / [`Repository::try_replace`] call.
#[napi(object)]
pub struct ReplaceResult {
    /// The object id of the freshly written replacement content.
    pub replacement_id: String,
    /// The replace reference that anchors the revision history (e.g. `refs/replace/<oid|path>`).
    pub reference: String,
    /// The complete revision manifest after the replace.
    pub manifest: ReplaceManifest,
}

/// Internal JSON shape persisted in the canonical `refs/replace/*` manifest blob.
#[derive(Serialize, Deserialize, Default)]
struct Manifest {
    /// The original object id (stable id for the whole revision history).
    #[serde(rename = "$ref", skip_serializing_if = "Option::is_none", default)]
    ref_: Option<String>,
    #[serde(default)]
    id: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    path: Option<String>,
    /// Latest revision content id.
    head: String,
    /// Ordered revision nodes, oldest first.
    #[serde(default)]
    revisions: Vec<Replace>,
}

impl From<Manifest> for ReplaceManifest {
    fn from(m: Manifest) -> Self {
        ReplaceManifest {
            id: m.id,
            path: m.path,
            r#type: m.kind,
            head: m.head,
            revisions: m.revisions,
        }
    }
}

/// Resolve the effective `core.useReplaceRefs` setting (`true` when unset).
///
/// `GIT_NO_REPLACE_OBJECTS` (the env form of `git --no-replace-objects`) disables
/// replacement resolution entirely when present.
fn use_replace_refs(r: &gix::Repository) -> bool {
    if std::env::var_os("GIT_NO_REPLACE_OBJECTS").is_some() {
        return false;
    }
    r.config_snapshot()
        .string("core.useReplaceRefs")
        .map(|v| v.to_string())
        .map(|s| s == "true" || s == "1")
        .unwrap_or(true)
}

/// Read the author identity for a new revision from the repository config.
fn author_info(r: &gix::Repository) -> (String, String) {
    let cfg = r.config_snapshot();
    let name = cfg
        .string("user.name")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let email = cfg
        .string("user.email")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "unknown@example.com".to_string());
    (name, email)
}

/// Current time as an RFC-3339 string, used for `ReplaceNode.updated_at`.
fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Write `bytes` as a new object of the same kind as `original`.
///
/// `git replace` requires a replacement to have the same type as the object it
/// replaces, so when the incoming content parses as the original's kind we write it
/// as that kind (commit/tree/tag). Otherwise — e.g. for arbitrary payloads that do
/// not parse — we fall back to a plain blob so the replace still succeeds.
fn write_same_object(
    r: &gix::Repository,
    original: &gix::Object,
    bytes: Vec<u8>,
) -> napi::Result<gix::ObjectId> {
    let hash = r.object_hash();
    let typed: Option<gix::ObjectId> = match original.kind {
        gix::object::Kind::Commit => gix::objs::CommitRef::from_bytes(&bytes, hash)
            .ok()
            .and_then(|c| r.write_object(&c).ok().map(|id| id.detach())),
        gix::object::Kind::Tree => gix::objs::TreeRef::from_bytes(&bytes, hash)
            .ok()
            .and_then(|t| r.write_object(&t).ok().map(|id| id.detach())),
        gix::object::Kind::Tag => gix::objs::TagRef::from_bytes(&bytes, hash)
            .ok()
            .and_then(|t| r.write_object(&t).ok().map(|id| id.detach())),
        gix::object::Kind::Blob => None,
    };
    match typed {
        Some(id) => Ok(id),
        None => r
            .write_blob(bytes)
            .map_err(|e| err("write_blob", e))
            .map(|id| id.detach()),
    }
}

/// Resolve the tree whose entries should be searched when deciding whether a
/// replaced object is a tracked file.
///
/// Per the spec the entry must be committed in a versioned tree under
/// `refs/heads/*`, preferably the tree of the branch configured as
/// `init.defaultBranch`; `HEAD` is used as the fallback.
fn entry_tree_id(r: &gix::Repository) -> Option<gix::ObjectId> {
    if let Some(branch) = r.config_snapshot().string("init.defaultBranch") {
        let name = format!("refs/heads/{}", branch.to_string());
        if let Ok(Some(reference)) = r.try_find_reference(name.as_str()) {
            if let gix::refs::TargetRef::Object(id) = reference.target() {
                return Some(id.to_owned());
            }
        }
    }
    match r.head_tree_id() {
        Ok(id) => Some(id.detach()),
        Err(_) => None,
    }
}

/// Resolve the commit that anchors the entry tree — the `init.defaultBranch` commit when
/// configured, otherwise `HEAD`. Used to enrich entry-type replaces with commit provenance.
fn entry_head_commit(r: &gix::Repository) -> Option<gix::Commit> {
    if let Some(branch) = r.config_snapshot().string("init.defaultBranch") {
        let name = format!("refs/heads/{}", branch);
        if let Ok(Some(mut reference)) = r.try_find_reference(name.as_str()) {
            if let Ok(commit) = reference.peel_to_commit() {
                return Some(commit);
            }
        }
    }
    r.head_commit().ok()
}

/// For entry-type replaces, return the author/committer provenance of the commit that
/// contains the replaced entry, so it can be recorded on the replacement node:
/// `(authorName, authorEmail, committerName, committerEmail, commitTimestamp)`.
fn entry_provenance(
    r: &gix::Repository,
) -> Option<(
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
)> {
    let commit = entry_head_commit(r)?;
    let author = commit.author().ok()?;
    let committer = commit.committer().ok()?;
    let commit_timestamp = commit.time().ok().map(|t| commit_time_iso(t.seconds));
    Some((
        String::from_utf8_lossy(author.name).into_owned(),
        String::from_utf8_lossy(author.email).into_owned(),
        Some(String::from_utf8_lossy(committer.name).into_owned()),
        Some(String::from_utf8_lossy(committer.email).into_owned()),
        commit_timestamp,
    ))
}

/// Extract `(authorName, authorEmail, committerName, committerEmail, commitTimestamp)` from a
/// commit object, used as replacement-node provenance for `commit`-type replaces.
fn commit_metadata(
    commit: &gix::objs::CommitRef,
) -> (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    let author = match commit.author() {
        Ok(a) => a,
        Err(_) => return (String::new(), String::new(), None, None, None),
    };
    let committer = match commit.committer() {
        Ok(c) => c,
        Err(_) => return (String::new(), String::new(), None, None, None),
    };
    let commit_timestamp = commit.time().ok().map(|t| commit_time_iso(t.seconds));
    (
        String::from_utf8_lossy(author.name).into_owned(),
        String::from_utf8_lossy(author.email).into_owned(),
        Some(String::from_utf8_lossy(committer.name).into_owned()),
        Some(String::from_utf8_lossy(committer.email).into_owned()),
        commit_timestamp,
    )
}

/// Format a git `time.seconds` (UTC) as an RFC-3339 string, matching `updated_at`.
fn commit_time_iso(secs: i64) -> String {
    use chrono::TimeZone;
    match chrono::Utc.timestamp_opt(secs, 0).single() {
        Some(dt) => dt.to_rfc3339(),
        None => secs.to_string(),
    }
}

/// Walk the current tree looking for an entry whose blob id equals `target`,
/// returning its repository-relative path (slash separated). Used to decide whether
/// the replaced object is a tracked file (so a `refs/replace/<path>` alias can be
/// created alongside `refs/replace/<oid>`).
fn find_entry_path(r: &gix::Repository, target: &gix::ObjectId) -> Option<String> {
    let tree_id = entry_tree_id(r)?;
    find_entry_path_in_tree(r, tree_id, target, String::new())
}

fn find_entry_path_in_tree(
    r: &gix::Repository,
    tree_id: gix::ObjectId,
    target: &gix::ObjectId,
    prefix: String,
) -> Option<String> {
    let tree_obj = r.find_tree(tree_id).ok()?;
    let tree = tree_obj.decode().ok()?;
    for entry in &tree.entries {
        let name = lossy(&entry.filename);
        let full = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if entry.mode.is_tree() {
            if let Some(found) = find_entry_path_in_tree(r, entry.oid.to_owned(), target, full) {
                return Some(found);
            }
        } else if entry.oid.to_owned() == *target {
            return Some(full);
        }
    }
    None
}

/// Read-side helper: if a replace reference exists for `object_id`, return the object
/// id of the latest revision content (`head`). Returns `None` when the machinery is
/// disabled (`core.useReplaceRefs=false`) or no replacement is recorded.
///
/// `catFile` / `readBlob` call this so that, by default, reading a replaced object
/// yields the latest replacement content.
pub(crate) fn resolve_replace_head(
    repo: &Arc<gix::ThreadSafeRepository>,
    object_id: &gix::ObjectId,
) -> Option<gix::ObjectId> {
    let r = thread_local(repo);
    if !use_replace_refs(&r) {
        return None;
    }
    let ref_name = format!("refs/replace/{object_id}");
    let reference = match r.try_find_reference(ref_name.as_str()) {
        Ok(Some(reference)) => reference,
        _ => return None,
    };
    let target = match reference.target() {
        gix::refs::TargetRef::Object(id) => id.to_owned(),
        gix::refs::TargetRef::Symbolic(_) => return None,
    };
    let manifest_obj = r.find_object(target).ok()?.detach();
    let manifest: Manifest = serde_json::from_slice(&manifest_obj.data).ok()?;
    gix::ObjectId::from_hex(gix::bstr::BStr::new(manifest.head.as_bytes())).ok()
}

/// Core replace logic, shared by `replace` and `try_replace`.
fn replace_inner(
    repo: &Arc<gix::ThreadSafeRepository>,
    object: String,
    content: Either<Buffer, String>,
    options: Option<ReplaceOptions>,
) -> napi::Result<ReplaceResult> {
    let r = thread_local(repo);
    let force = options.as_ref().and_then(|o| o.force).unwrap_or(false);
    let use_replace = match options.as_ref().and_then(|o| o.use_replace_refs) {
        Some(v) => v,
        None => use_replace_refs(&r),
    };
    let msg = options
        .as_ref()
        .and_then(|o| o.message.clone())
        .unwrap_or_else(|| "replace object".to_string());

    // Resolve the original object to its oid + kind.
    let original_id: gix::ObjectId = r
        .rev_parse_single(object.as_str())
        .map_err(|e| err("rev_parse_single", e))?
        .into();
    let original = r
        .find_object(original_id)
        .map_err(|e| err("find_object", e))?;
    let kind = kind_to_string(original.kind);

    let bytes = match content {
        Either::A(b) => b.to_vec(),
        Either::B(s) => s.into_bytes(),
    };

    let (author_name, author_email) = author_info(&r);
    let updated_at = now_iso();

    // core.useReplaceRefs disabled → write the new content directly (same type as the
    // original) and return it; no `refs/replace/*` history is recorded.
    if !use_replace {
        let new_id = write_same_object(&r, &original, bytes)?;
        return Ok(ReplaceResult {
            replacement_id: new_id.to_string(),
            reference: original_id.to_string(),
            manifest: ReplaceManifest {
                id: original_id.to_string(),
                path: None,
                r#type: kind,
                head: new_id.to_string(),
                revisions: vec![Replace {
                    r#this: new_id.to_string(),
                    prev: None,
                    next: None,
                    head: Some(new_id.to_string()),
                    r#authorName: author_name,
                    r#authorEmail: author_email,
                    r#updated_at: updated_at,
                    r#committerName: None,
                    r#committerEmail: None,
                    r#commitTimestamp: None,
                }],
            },
        });
    }

    // Decide whether the object is a tracked entry in the current tree.
    let entry_path = find_entry_path(&r, &original_id);

    // Enrich the revision node with commit provenance based on the replaced object's kind:
    //  - `commit`: the original commit's own author/committer/timestamp.
    //  - `tree` / `entry` (a blob committed in a tree): the author/committer/timestamp of the
    //    commit that contains the tree (preferring `init.defaultBranch`, else `HEAD`).
    //  - plain `blob` (not a tracked entry): no commit provenance is available.
    let identity = || (author_name.clone(), author_email.clone(), None, None, None);
    let (node_author_name, node_author_email, committer_name, committer_email, commit_timestamp) =
        match original.kind {
            gix::object::Kind::Commit => original
                .try_to_commit_ref()
                .ok()
                .map(|c| commit_metadata(&c))
                .unwrap_or_else(identity),
            gix::object::Kind::Tree | gix::object::Kind::Blob if entry_path.is_some() => {
                entry_provenance(&r).unwrap_or_else(identity)
            }
            _ => identity(),
        };

    // Primary replace reference, keyed by the original object id.
    let ref_name = format!("refs/replace/{original_id}");
    let (existing_target, constraint) = match r.try_find_reference(ref_name.as_str()) {
        Ok(Some(reference)) => {
            let target = match reference.target() {
                gix::refs::TargetRef::Object(id) => id.to_owned(),
                gix::refs::TargetRef::Symbolic(_) => {
                    return Err(err_str(
                        "replace",
                        format!("symbolic replace ref '{ref_name}' is not supported"),
                    ));
                }
            };
            (Some(target), gix::refs::transaction::PreviousValue::Any)
        }
        Ok(None) => (None, gix::refs::transaction::PreviousValue::MustNotExist),
        Err(e) => return Err(err("try_find_reference", e)),
    };
    if existing_target.is_some() && !force {
        return Err(err_str(
            "replace",
            format!("replace reference '{ref_name}' already exists (use force to overwrite)"),
        ));
    }

    // Stage the new content as an object of the same type as the original.
    let new_id = write_same_object(&r, &original, bytes)?;

    // Seed or load the canonical manifest.
    let mut manifest: Manifest = match existing_target {
        Some(target) => {
            let obj = r
                .find_object(target)
                .map_err(|e| err("find_object", e))?
                .detach();
            serde_json::from_slice(&obj.data)
                .map_err(|e| err_str("replace", format!("invalid manifest: {e}")))?
        }
        None => Manifest {
            ref_: Some(original_id.to_string()),
            id: original_id.to_string(),
            kind: replace_type(&entry_path, &kind),
            path: entry_path.clone(),
            head: original_id.to_string(),
            revisions: Vec::new(),
        },
    };

    // The previous head (a content oid) becomes `prev` of the new node.
    let prev_oid: Option<String> = if manifest.revisions.is_empty() {
        None
    } else {
        Some(manifest.head.clone())
    };

    let node = Replace {
        r#this: new_id.to_string(),
        prev: prev_oid,
        next: None,
        head: Some(new_id.to_string()),
        r#authorName: node_author_name,
        r#authorEmail: node_author_email,
        r#updated_at: updated_at.clone(),
        r#committerName: committer_name,
        r#committerEmail: committer_email,
        r#commitTimestamp: commit_timestamp,
    };

    // Persist the individual revision node at refs/replace/<newOid> (target = node blob).
    let node_json = serde_json::to_vec(&node).map_err(|e| err_str("replace", e))?;
    let node_blob = r.write_blob(node_json).map_err(|e| err("write_blob", e))?;
    r.reference(
        format!("refs/replace/{new_id}").as_str(),
        node_blob,
        gix::refs::transaction::PreviousValue::MustNotExist,
        msg.as_str(),
    )
    .map_err(|e| err("reference", e))?;

    // Link the previous head node's `next` to this new node so the chain is walkable.
    if let Some(prev) = node.prev.clone() {
        let prev_ref = format!("refs/replace/{prev}");
        if let Ok(Some(reference)) = r.try_find_reference(prev_ref.as_str()) {
            if let gix::refs::TargetRef::Object(prev_target) = reference.target() {
                if let Some(data) = r
                    .find_object(prev_target.to_owned())
                    .ok()
                    .map(|o| o.detach().data)
                {
                    if let Ok(mut prev_node) = serde_json::from_slice::<Replace>(&data) {
                        prev_node.next = Some(new_id.to_string());
                        let updated =
                            serde_json::to_vec(&prev_node).map_err(|e| err_str("replace", e))?;
                        let updated_blob =
                            r.write_blob(updated).map_err(|e| err("write_blob", e))?;
                        r.reference(
                            prev_ref.as_str(),
                            updated_blob,
                            gix::refs::transaction::PreviousValue::Any,
                            msg.as_str(),
                        )
                        .map_err(|e| err("reference", e))?;
                    }
                }
            }
        }
    }

    // Advance the canonical manifest's head and append the new revision node.
    manifest.head = new_id.to_string();
    manifest.revisions.push(node);
    if manifest.path.is_none() {
        manifest.path = entry_path.clone();
    }
    if manifest.kind.is_empty() {
        manifest.kind = replace_type(&entry_path, &kind);
    }
    if manifest.id.is_empty() {
        manifest.id = original_id.to_string();
    }
    if manifest.ref_.is_none() {
        manifest.ref_ = Some(original_id.to_string());
    }

    // Persist the manifest blob and point the replace reference(s) at it.
    let manifest_json = serde_json::to_vec(&manifest).map_err(|e| err_str("replace", e))?;
    let manifest_id = r
        .write_blob(manifest_json)
        .map_err(|e| err("write_blob", e))?;

    r.reference(ref_name.as_str(), manifest_id, constraint, msg.as_str())
        .map_err(|e| err("reference", e))?;

    if let Some(path) = &entry_path {
        let path_ref = format!("refs/replace/{path}");
        r.reference(
            path_ref.as_str(),
            manifest_id,
            gix::refs::transaction::PreviousValue::Any,
            msg.as_str(),
        )
        .map_err(|e| err("reference", e))?;
    }

    // Point `refs/objects/<originalOid>` at the latest replacement content, and record a
    // self-referencing node `refs/objects/<newOid>` for this specific revision.
    let objects_latest = format!("refs/objects/{original_id}");
    let objects_node = format!("refs/objects/{new_id}");
    r.reference(
        objects_latest.as_str(),
        new_id,
        gix::refs::transaction::PreviousValue::Any,
        msg.as_str(),
    )
    .map_err(|e| err("reference", e))?;
    r.reference(
        objects_node.as_str(),
        new_id,
        gix::refs::transaction::PreviousValue::MustNotExist,
        msg.as_str(),
    )
    .map_err(|e| err("reference", e))?;

    Ok(ReplaceResult {
        replacement_id: new_id.to_string(),
        reference: ref_name,
        manifest: manifest.into(),
    })
}

/// The `type` discriminator for a replace record: `"entry"` when the replaced
/// object is a tracked tree file, otherwise the object's own git kind.
fn replace_type(entry_path: &Option<String>, kind: &str) -> String {
    match entry_path {
        Some(_) => "entry".to_string(),
        None => kind.to_string(),
    }
}

#[napi]
impl Repository {
    /// Replace `object` (given by id or rev-spec) with `content`, recording a
    /// revision history under `refs/replace/*`.
    ///
    /// When `core.useReplaceRefs` is `true` (the default) this writes `content` as a new
    /// object **of the same type** as `original` (the replaceOid), stores the individual
    /// `Replace` metadata node at `refs/replace/<replaceOid>`, and creates/updates the
    /// root `ReplaceManifest` at `refs/replace/<origOid>` (and, when the object is a
    /// tracked file, also at `refs/replace/<path>`), appending the new node to its
    /// `revisions` array. Content pointers are kept under `refs/objects/<origOid>`
    /// (latest, i.e. replaceOid) and `refs/objects/<replaceOid>` (self-reference).
    /// Subsequent `catFile` / `readBlob` calls on the original id transparently return
    /// the latest replacement content.
    ///
    /// Throws if a `refs/replace/<oid>` already exists and `options.force` is unset.
    #[napi]
    pub fn replace(
        &self,
        object: String,
        content: Either<Buffer, String>,
        options: Option<ReplaceOptions>,
    ) -> napi::Result<ReplaceResult> {
        replace_inner(&self.inner, object, content, options)
    }

    /// Like [`replace`](Self::replace) but returns `null` (instead of throwing) when
    /// the replace reference already exists and `force` is not set.
    #[napi(ts_return_type = "ReplaceResult | null")]
    pub fn try_replace(
        &self,
        object: String,
        content: Either<Buffer, String>,
        options: Option<ReplaceOptions>,
    ) -> napi::Result<Option<ReplaceResult>> {
        let force = options.as_ref().and_then(|o| o.force).unwrap_or(false);
        if !force {
            let r = thread_local(&self.inner);
            let oid: gix::ObjectId = match r.rev_parse_single(object.as_str()) {
                Ok(id) => id.into(),
                Err(_) => return Ok(None),
            };
            let ref_name = format!("refs/replace/{oid}");
            if r.try_find_reference(ref_name.as_str())
                .map(|o| o.is_some())
                .unwrap_or(false)
            {
                return Ok(None);
            }
        }
        Ok(Some(replace_inner(&self.inner, object, content, options)?))
    }

    /// Read an individual `Replace` (a.k.a. `Revision`) node.
    ///
    /// Every revision of an artifact is stored on its own at
    /// `refs/replace/<replaceOid>` — the id of the new replacement content — carrying
    /// `this`, `prev`, `next` and `head` references plus author metadata. This lets a
    /// consumer walk the history node by node instead of only iterating the manifest's
    /// flat `revisions` array. Returns `null` when no such node exists (or the stored
    /// blob is not a revision node).
    #[napi(ts_return_type = "Revision | null")]
    pub fn get_replace_node(&self, oid: String) -> napi::Result<Option<Replace>> {
        let r = thread_local(&self.inner);
        let ref_name = format!("refs/replace/{oid}");
        let reference = match r.try_find_reference(ref_name.as_str()) {
            Ok(Some(reference)) => reference,
            _ => return Ok(None),
        };
        let target = match reference.target() {
            gix::refs::TargetRef::Object(id) => id.to_owned(),
            gix::refs::TargetRef::Symbolic(_) => return Ok(None),
        };
        let node_obj = match r.find_object(target) {
            Ok(obj) => obj.detach(),
            Err(_) => return Ok(None),
        };
        Ok(serde_json::from_slice::<Replace>(&node_obj.data).ok())
    }
}
