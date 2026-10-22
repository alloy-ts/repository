use std::sync::Arc;

use gix::objs::Write;
use napi::Result;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use crate::blob::{Blob, build_blob};
use crate::commit::Commit;
use crate::error::{err, err_str, parse_oid, thread_local};
use crate::repository::Repository;
use crate::tag::{Tag, build_tag};
use crate::tree::{Tree, Entry, build_tree, build_entry, find_commit_metadata_for_tree_or_entry, TreeItemInput};

pub(crate) fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A kinded wrapper around any git object, with downcast helpers.
#[napi]
pub struct GitObject {
    repo: Arc<gix::ThreadSafeRepository>,
    id: gix::ObjectId,
    kind: String,
}

#[napi]
impl GitObject {
    #[napi(getter)]
    pub fn kind(&self) -> String {
        self.kind.clone()
    }

    #[napi(getter)]
    pub fn id(&self) -> String {
        self.id.to_string()
    }

    /// The raw, decompressed object body (without the loose-object header).
    #[napi]
    pub fn data(&self) -> Result<Buffer> {
        let r = thread_local(&self.repo);
        let obj = r
            .find_object(self.id)
            .map_err(|e| err("find_object", e))?
            .detach();
        Ok(Buffer::from(obj.data))
    }

    #[napi]
    pub fn as_commit(&self) -> Result<Option<Commit>> {
        if self.kind != "commit" {
            return Ok(None);
        }
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        Ok(Some(crate::commit::build_commit(&self.repo, commit)?))
    }

    #[napi]
    pub fn as_tree(&self) -> Result<Option<Tree>> {
        if self.kind != "tree" {
            return Ok(None);
        }
        let r = thread_local(&self.repo);
        let tree = r.find_tree(self.id).map_err(|e| err("find_tree", e))?;
        Ok(Some(build_tree(&self.repo, tree.id)?))
    }

    #[napi]
    pub fn as_blob(&self) -> Result<Option<Blob>> {
        if self.kind != "blob" {
            return Ok(None);
        }
        Ok(Some(build_blob(&self.repo, self.id)))
    }

    #[napi]
    pub fn as_tag(&self) -> Result<Option<Tag>> {
        if self.kind != "tag" {
            return Ok(None);
        }
        let r = thread_local(&self.repo);
        let tag = r.find_tag(self.id).map_err(|e| err("find_tag", e))?;
        Ok(Some(build_tag(&self.repo, tag)?))
    }

    /// Expose as an Entry if this object is a committed tree entry.
    #[napi]
    pub fn as_entry(&self) -> Result<Option<Entry>> {
        let r = thread_local(&self.repo);
        let head_commit = match r.head_commit() {
            Ok(c) => c,
            Err(_) => return Ok(None),
        };
        let head_tree = match head_commit.tree() {
            Ok(t) => t,
            Err(_) => return Ok(None),
        };
        if let Ok(Some(entry)) = head_tree.lookup_entry_by_path(std::path::Path::new(&self.id.to_string())) {
            let mode_str = match entry.mode().into() {
                gix::object::tree::EntryKind::Blob => "100644",
                gix::object::tree::EntryKind::BlobExecutable => "100755",
                gix::object::tree::EntryKind::Tree => "040000",
                gix::object::tree::EntryKind::Link => "120000",
                gix::object::tree::EntryKind::Commit => "160000",
            }.to_string();
            return Ok(Some(build_entry(&self.repo, head_tree.id, lossy(entry.filename()), mode_str, entry.id().detach())));
        }
        Ok(None)
    }

    /// Follow tag/commit objects until a commit is reached, returning it.
    #[napi]
    pub fn peel_to_commit(&self) -> Result<Commit> {
        let r = thread_local(&self.repo);
        let obj = r.find_object(self.id).map_err(|e| err("find_object", e))?;
        let peeled = obj
            .peel_to_kind(gix::object::Kind::Commit)
            .map_err(|e| err("peel_to_commit", e))?;
        Ok(Commit {
            repo: Arc::clone(&self.repo),
            id: peeled.id,
        })
    }

    /// Follow tag/commit objects until a tree is reached, returning it.
    #[napi]
    pub fn peel_to_tree(&self) -> Result<Tree> {
        let r = thread_local(&self.repo);
        let obj = r.find_object(self.id).map_err(|e| err("find_object", e))?;
        let peeled = obj
            .peel_to_kind(gix::object::Kind::Tree)
            .map_err(|e| err("peel_to_tree", e))?;
        build_tree(&self.repo, peeled.id)
    }
}

pub fn build_object(
    repo: &Arc<gix::ThreadSafeRepository>,
    object: gix::Object,
) -> Result<GitObject> {
    let kind = match object.kind {
        gix::object::Kind::Commit => "commit",
        gix::object::Kind::Tree => "tree",
        gix::object::Kind::Blob => "blob",
        gix::object::Kind::Tag => "tag",
    }
    .to_string();
    Ok(GitObject {
        repo: Arc::clone(repo),
        id: object.id,
        kind,
    })
}

/// An object header: its kind, the size of its encoded data in bytes, and where it was found.
#[napi(object)]
pub struct ObjectHeader {
    pub kind: String,
    pub size: f64,
    pub is_packed: bool,
    pub num_deltas: Option<f64>,
}

impl From<gix::odb::find::Header> for ObjectHeader {
    fn from(h: gix::odb::find::Header) -> Self {
        match h {
            gix::odb::find::Header::Loose { kind, size } => ObjectHeader {
                kind: kind_to_string(kind),
                size: size as f64,
                is_packed: false,
                num_deltas: None,
            },
            gix::odb::find::Header::Packed(out) => ObjectHeader {
                kind: kind_to_string(out.kind),
                size: out.object_size as f64,
                is_packed: true,
                num_deltas: Some(out.num_deltas as f64),
            },
        }
    }
}

/// An object's kind and its raw, decompressed body (without the loose-object header).
#[napi(object)]
pub struct ObjectData {
    pub kind: String,
    pub data: Buffer,
}

pub(crate) fn kind_to_string(kind: gix::object::Kind) -> String {
    match kind {
        gix::object::Kind::Commit => "commit",
        gix::object::Kind::Tree => "tree",
        gix::object::Kind::Blob => "blob",
        gix::object::Kind::Tag => "tag",
    }
    .to_string()
}

pub(crate) fn string_to_kind(s: &str) -> Result<gix::object::Kind> {
    match s.to_lowercase().as_str() {
        "commit" => Ok(gix::object::Kind::Commit),
        "tree" => Ok(gix::object::Kind::Tree),
        "blob" => Ok(gix::object::Kind::Blob),
        "tag" => Ok(gix::object::Kind::Tag),
        other => Err(err_str(
            "write_object",
            format!("invalid object kind {other}"),
        )),
    }
}

/// Replacement record for a single replacement step.
#[napi(object)]
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ReplaceRecord {
    pub init_oid: String,
    pub replace_oid: String,
    pub kind: String,
    pub path: Option<String>,
    pub committer_name: Option<String>,
    pub committer_email: Option<String>,
    pub author_name: Option<String>,
    pub author_email: Option<String>,
    pub commit_timestamp: Option<f64>,
    pub timestamp: f64,
}

/// Replace manifest holding history of replacements for a canonical initial object ID.
#[napi(object)]
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ReplaceManifest {
    pub init_oid: String,
    pub latest_replace_oid: String,
    pub kind: String,
    pub path: Option<String>,
    pub history: Vec<ReplaceRecord>,
}

/// Options for replacing an object.
#[napi(object)]
pub struct ReplaceOptions {
    pub content: Option<Buffer>,
    pub entries: Option<Vec<TreeItemInput>>,
    pub path: Option<String>,
    pub author_name: Option<String>,
    pub author_email: Option<String>,
    pub committer_name: Option<String>,
    pub committer_email: Option<String>,
    pub commit_timestamp: Option<f64>,
}

#[napi]
impl Repository {
    #[napi]
    pub fn find_object(&self, id: String) -> Result<GitObject> {
        let r = thread_local(&self.inner);
        let object = r
            .find_object(parse_oid(&id)?)
            .map_err(|e| err("find_object", e))?;
        build_object(&self.inner, object)
    }

    #[napi]
    pub fn object_exists(&self, id: String) -> bool {
        let r = thread_local(&self.inner);
        match parse_oid(&id) {
            Ok(oid) => r.has_object(oid),
            Err(_) => false,
        }
    }

    #[napi]
    pub fn peel(&self, id: String, kind: String) -> Result<String> {
        let r = thread_local(&self.inner);
        let oid = parse_oid(&id)?;
        let target_kind = match kind.to_lowercase().as_str() {
            "commit" => gix::objs::Kind::Commit,
            "tree" => gix::objs::Kind::Tree,
            "blob" => gix::objs::Kind::Blob,
            "tag" => gix::objs::Kind::Tag,
            _ => return Err(err_str("peel", format!("invalid target kind {kind}"))),
        };
        let obj = r.find_object(oid).map_err(|e| err("find_object", e))?;
        let peeled = obj.peel_to_kind(target_kind).map_err(|e| err("peel", e))?;
        Ok(peeled.id().to_string())
    }

    #[napi]
    pub fn try_find_object(&self, id: String) -> Result<Option<GitObject>> {
        let r = thread_local(&self.inner);
        match parse_oid(&id) {
            Ok(oid) => match r.find_object(oid) {
                Ok(obj) => Ok(Some(build_object(&self.inner, obj)?)),
                Err(_) => Ok(None),
            },
            Err(_) => Ok(None),
        }
    }

    #[napi]
    pub fn write_object(&self, kind: String, data: Buffer) -> Result<String> {
        let r = thread_local(&self.inner);
        let kind = string_to_kind(&kind)?;
        let data = data.to_vec();
        let mut buf = data.as_slice();
        let id = r
            .write_stream(kind, data.len() as u64, &mut buf)
            .map_err(|e| err("write_object", e))?;
        Ok(id.to_string())
    }

    #[napi]
    pub fn read_object(&self, id: String) -> Result<ObjectData> {
        let r = thread_local(&self.inner);
        let obj = r
            .find_object(parse_oid(&id)?)
            .map_err(|e| err("find_object", e))?
            .detach();
        Ok(ObjectData {
            kind: kind_to_string(obj.kind),
            data: Buffer::from(obj.data),
        })
    }

    #[napi]
    pub fn compute_hash(&self, kind: String, data: Buffer) -> Result<String> {
        let r = thread_local(&self.inner);
        let kind = string_to_kind(&kind)?;
        let id = gix::objs::compute_hash(r.object_hash(), kind, &data)
            .map_err(|e| err("compute_hash", e))?;
        Ok(id.to_string())
    }

    #[napi]
    pub fn find_header(&self, id: String) -> Result<ObjectHeader> {
        let r = thread_local(&self.inner);
        let header = r
            .find_header(parse_oid(&id)?)
            .map_err(|e| err("find_header", e))?;
        Ok(ObjectHeader {
            kind: kind_to_string(header.kind()),
            size: header.size() as f64,
            is_packed: false,
            num_deltas: None,
        })
    }

    #[napi]
    pub fn try_find_header(&self, id: String) -> Result<Option<ObjectHeader>> {
        let r = thread_local(&self.inner);
        match parse_oid(&id) {
            Ok(oid) => match r.find_header(oid) {
                Ok(header) => Ok(Some(ObjectHeader {
                    kind: kind_to_string(header.kind()),
                    size: header.size() as f64,
                    is_packed: false,
                    num_deltas: None,
                })),
                Err(_) => Ok(None),
            },
            Err(_) => Ok(None),
        }
    }

    #[napi]
    pub fn object_cache_size(&self, bytes: f64) -> Result<()> {
        let mut r = thread_local(&self.inner);
        r.object_cache_size(bytes as usize);
        Ok(())
    }

    #[napi]
    pub fn object_cache_size_if_unset(&self, bytes: f64) -> Result<()> {
        let mut r = thread_local(&self.inner);
        r.object_cache_size_if_unset(bytes as usize);
        Ok(())
    }

    #[napi]
    pub fn compute_object_cache_size_for_tree_diffs(&self) -> Result<f64> {
        let r = thread_local(&self.inner);
        let index = r.index().map_err(|e| err("index", e))?;
        Ok(r.compute_object_cache_size_for_tree_diffs(&*index) as f64)
    }

    #[napi]
    pub fn with_object_memory(&self) -> Result<()> {
        let r = thread_local(&self.inner);
        r.with_object_memory();
        Ok(())
    }

    /// Replace an object (`blob`, `tree`, `commit`, `tag`, `entry`) with a new replacement object.
    #[napi]
    pub fn replace_object(
        &self,
        target_id: String,
        kind_input: Option<String>,
        options: Option<ReplaceOptions>,
    ) -> Result<ReplaceManifest> {
        let r = thread_local(&self.inner);
        let opts = options.unwrap_or_else(|| ReplaceOptions {
            content: None,
            entries: None,
            path: None,
            author_name: None,
            author_email: None,
            committer_name: None,
            committer_email: None,
            commit_timestamp: None,
        });

        let (init_oid, detected_kind) = match parse_oid(&target_id) {
            Ok(oid) => {
                if let Ok(obj) = r.find_object(oid) {
                    (oid.to_string(), kind_to_string(obj.kind))
                } else {
                    (target_id.clone(), kind_input.clone().unwrap_or_else(|| "blob".into()))
                }
            }
            Err(_) => (target_id.clone(), kind_input.clone().unwrap_or_else(|| "blob".into())),
        };

        let kind = if opts.path.is_some() {
            "entry".to_string()
        } else if let Some(ref k) = kind_input {
            k.clone()
        } else {
            detected_kind
        };

        let commit_meta = find_commit_metadata_for_tree_or_entry(
            &r,
            parse_oid(&init_oid).ok(),
            parse_oid(&init_oid).ok(),
        );

        let author_name = opts.author_name.or_else(|| commit_meta.as_ref().map(|m| m.author_name.clone()));
        let author_email = opts.author_email.or_else(|| commit_meta.as_ref().map(|m| m.author_email.clone()));
        let committer_name = opts.committer_name.or_else(|| commit_meta.as_ref().map(|m| m.committer_name.clone()));
        let committer_email = opts.committer_email.or_else(|| commit_meta.as_ref().map(|m| m.committer_email.clone()));
        let commit_timestamp = opts.commit_timestamp.or_else(|| commit_meta.as_ref().map(|m| m.commit_timestamp));

        let replace_oid = if kind == "tree" {
            let tree_entries = opts.entries.unwrap_or_default();
            self.write_tree(tree_entries)?
        } else if kind == "commit" {
            let buf = opts.content.unwrap_or_else(|| Buffer::from(vec![]));
            self.write_object("commit".into(), buf)?
        } else {
            let buf = opts.content.unwrap_or_else(|| Buffer::from(vec![]));
            self.write_blob(buf)?
        };

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);

        let new_record = ReplaceRecord {
            init_oid: init_oid.clone(),
            replace_oid: replace_oid.clone(),
            kind: kind.clone(),
            path: opts.path.clone(),
            committer_name,
            committer_email,
            author_name,
            author_email,
            commit_timestamp,
            timestamp: now,
        };

        let record_json = serde_json::to_string(&new_record)
            .map_err(|e| err_str("replace_object", format!("serialize record: {e}")))?;
        let record_blob_id = r.write_blob(record_json.as_bytes())
            .map_err(|e| err("replace_object", e))?;
        let _ = r.reference(
            format!("refs/replace/{replace_oid}").as_str(),
            record_blob_id,
            gix::refs::transaction::PreviousValue::Any,
            "replace: create replace record",
        );

        let manifest_ref = format!("refs/replace/{init_oid}");
        let mut history = Vec::new();

        if let Ok(Some(existing_blob)) = r.try_find_reference(manifest_ref.as_str()) {
            if let Some(id) = existing_blob.try_id() {
                if let Ok(blob) = r.find_blob(id.detach()) {
                    if let Ok(existing_manifest) = serde_json::from_slice::<ReplaceManifest>(&blob.data) {
                        history = existing_manifest.history;
                    }
                }
            }
        }

        history.push(new_record);

        let manifest = ReplaceManifest {
            init_oid: init_oid.clone(),
            latest_replace_oid: replace_oid.clone(),
            kind,
            path: opts.path,
            history,
        };

        let manifest_json = serde_json::to_string(&manifest)
            .map_err(|e| err_str("replace_object", format!("serialize manifest: {e}")))?;
        let manifest_blob_id = r.write_blob(manifest_json.as_bytes())
            .map_err(|e| err("replace_object", e))?;

        r.reference(
            manifest_ref.as_str(),
            manifest_blob_id,
            gix::refs::transaction::PreviousValue::Any,
            "replace: update manifest",
        )
        .map_err(|e| err("replace_object.ref", e))?;

        let obj_ref = format!("refs/objects/{init_oid}");
        let replace_blob_oid = parse_oid(&replace_oid)?;
        let _ = r.reference(
            obj_ref.as_str(),
            replace_blob_oid,
            gix::refs::transaction::PreviousValue::Any,
            "replace: update object ref",
        );

        Ok(manifest)
    }

    /// Read the replace manifest for an object ID from `refs/replace/{init_oid}`.
    #[napi]
    pub fn get_replace_manifest(&self, init_oid: String) -> Result<Option<ReplaceManifest>> {
        let r = thread_local(&self.inner);
        let manifest_ref = format!("refs/replace/{init_oid}");
        if let Ok(Some(rf)) = r.try_find_reference(manifest_ref.as_str()) {
            if let Some(id) = rf.try_id() {
                if let Ok(blob) = r.find_blob(id.detach()) {
                    if let Ok(manifest) = serde_json::from_slice::<ReplaceManifest>(&blob.data) {
                        return Ok(Some(manifest));
                    }
                }
            }
        }
        Ok(None)
    }
}
