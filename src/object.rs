use std::sync::Arc;

use gix::bstr::ByteSlice;
use gix::objs::Write;
use napi::Result;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use crate::commit::Commit;
use crate::error::{err, parse_oid, thread_local};
use crate::repository::{Repository, signature_from_dto};

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Build a NAPI [`Signature`] from raw actor parts.
pub(crate) fn to_signature(name: &[u8], email: &[u8], seconds: i64, offset: i32) -> Signature {
    Signature {
        name: lossy(name),
        email: lossy(email),
        time: seconds,
        timezone_offset: offset,
    }
}

fn mode_string(kind: gix::object::tree::EntryKind) -> String {
    match kind {
        gix::object::tree::EntryKind::Blob => "100644".to_string(),
        gix::object::tree::EntryKind::BlobExecutable => "100755".to_string(),
        gix::object::tree::EntryKind::Tree => "040000".to_string(),
        gix::object::tree::EntryKind::Link => "120000".to_string(),
        gix::object::tree::EntryKind::Commit => "160000".to_string(),
    }
}

/// An actor signature, mirroring `gix::actor::Signature`.
#[derive(Clone)]
#[napi(object)]
pub struct Signature {
    pub name: String,
    pub email: String,
    pub time: i64,
    pub timezone_offset: i32,
}

/// A git blob exposed to TypeScript.
#[napi]
pub struct Blob {
    repo: Arc<gix::ThreadSafeRepository>,
    id: gix::ObjectId,
}

#[napi]
impl Blob {
    /// Raw bytes of the blob.
    #[napi]
    pub fn content(&self) -> Result<Buffer> {
        let r = thread_local(&self.repo);
        let blob = r.find_blob(self.id).map_err(|e| err("find_blob", e))?;
        Ok(Buffer::from(blob.data.to_vec()))
    }

    /// The object id of this blob.
    #[napi(getter)]
    pub fn id(&self) -> String {
        self.id.to_string()
    }
}

/// A single entry of a git tree.
#[napi(object)]
pub struct TreeEntry {
    pub mode: String,
    pub id: String,
}

/// A git entry object (a file committed in a tree/commit).
#[derive(Clone)]
#[napi(object)]
pub struct Entry {
    pub path: String,
    pub mode: String,
    pub id: String,
    #[napi(js_name = "authorName")]
    pub author_name: Option<String>,
    #[napi(js_name = "authorEmail")]
    pub author_email: Option<String>,
    #[napi(js_name = "committerName")]
    pub committer_name: Option<String>,
    #[napi(js_name = "committerEmail")]
    pub committer_email: Option<String>,
    #[napi(js_name = "commitTimestamp")]
    pub commit_timestamp: Option<String>,
}

/// A git tree exposed to TypeScript.
#[napi]
pub struct Tree {
    repo: Arc<gix::ThreadSafeRepository>,
    id: gix::ObjectId,
}

#[napi]
impl Tree {
    /// The object id of this tree.
    #[napi(getter)]
    pub fn id(&self) -> String {
        self.id.to_string()
    }

    /// All entries of the tree.
    #[napi(getter)]
    pub fn entries(&self) -> Result<Vec<TreeEntry>> {
        let r = thread_local(&self.repo);
        let tree = r.find_tree(self.id).map_err(|e| err("find_tree", e))?;
        let tree = tree.decode().map_err(|e| err("decode", e))?;
        let mut out = Vec::new();
        for entry in &tree.entries {
            out.push(TreeEntry {
                mode: mode_string(entry.mode.into()),
                id: entry.oid.to_string(),
            });
        }
        Ok(out)
    }

    /// Look up a single entry by file name.
    #[napi]
    pub fn entry(&self, name: String) -> Result<Option<TreeEntry>> {
        let r = thread_local(&self.repo);
        let tree = r.find_tree(self.id).map_err(|e| err("find_tree", e))?;
        let tree = tree.decode().map_err(|e| err("decode", e))?;
        for entry in &tree.entries {
            if entry.filename.as_bytes() == name.as_bytes() {
                return Ok(Some(TreeEntry {
                    mode: mode_string(entry.mode.into()),
                    id: entry.oid.to_string(),
                }));
            }
        }
        Ok(None)
    }
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
        Ok(Tree {
            repo: Arc::clone(&self.repo),
            id: peeled.id,
        })
    }

    /// Replace this object's content with new content, returning the replace manifest.
    #[napi]
    pub fn replace(&self, new_content: Buffer, path: Option<String>) -> Result<crate::replace::ReplaceManifest> {
        let repo = Repository {
            inner: Arc::clone(&self.repo),
        };
        repo.replace(self.id.to_string(), new_content, path)
    }
}

/// A git tag object exposed to TypeScript.
#[napi]
pub struct Tag {
    repo: Arc<gix::ThreadSafeRepository>,
    id: gix::ObjectId,
}

#[napi]
impl Tag {
    #[napi(getter)]
    pub fn name(&self) -> Result<String> {
        let r = thread_local(&self.repo);
        let tag = r.find_tag(self.id).map_err(|e| err("find_tag", e))?;
        let tag = tag.decode().map_err(|e| err("decode", e))?;
        Ok(lossy(tag.name))
    }

    #[napi(getter)]
    pub fn message(&self) -> Result<String> {
        let r = thread_local(&self.repo);
        let tag = r.find_tag(self.id).map_err(|e| err("find_tag", e))?;
        let tag = tag.decode().map_err(|e| err("decode", e))?;
        Ok(lossy(tag.message))
    }

    /// The object id of this tag.
    #[napi(getter)]
    pub fn id(&self) -> String {
        self.id.to_string()
    }

    /// The id of the object this tag points to.
    #[napi(getter)]
    pub fn target_id(&self) -> Result<String> {
        let r = thread_local(&self.repo);
        let tag = r.find_tag(self.id).map_err(|e| err("find_tag", e))?;
        let tag = tag.decode().map_err(|e| err("decode", e))?;
        Ok(tag.target.to_string())
    }

    /// The tagger identity, if this is an annotated tag.
    #[napi]
    pub fn tagger(&self) -> Result<Option<Signature>> {
        let r = thread_local(&self.repo);
        let tag = r.find_tag(self.id).map_err(|e| err("find_tag", e))?;
        let tag = tag.decode().map_err(|e| err("decode", e))?;
        let tagger = tag.tagger().map_err(|e| err("tagger", e))?;
        match tagger {
            Some(s) => {
                let time = s.time().map_err(|e| err("tagger.time", e))?;
                Ok(Some(to_signature(
                    s.name,
                    s.email,
                    time.seconds,
                    time.offset,
                )))
            }
            None => Ok(None),
        }
    }
}

// ---- builders ---------------------------------------------------------------

pub fn build_blob(repo: &Arc<gix::ThreadSafeRepository>, id: gix::ObjectId) -> Blob {
    Blob {
        repo: Arc::clone(repo),
        id,
    }
}

pub fn build_tree(repo: &Arc<gix::ThreadSafeRepository>, id: gix::ObjectId) -> Result<Tree> {
    Ok(Tree {
        repo: Arc::clone(repo),
        id,
    })
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

pub fn build_tag(repo: &Arc<gix::ThreadSafeRepository>, tag: gix::Tag) -> Result<Tag> {
    Ok(Tag {
        repo: Arc::clone(repo),
        id: tag.id,
    })
}

/// An object header: its kind and the size of its encoded data in bytes.
#[napi(object)]
pub struct ObjectHeader {
    pub kind: String,
    pub size: f64,
}

/// Input structure for constructing a tree item.
#[derive(Clone)]
#[napi(object)]
pub struct TreeItemInput {
    pub path: String,
    pub mode: Option<String>,
    pub id: String,
}

/// A single edit applied by [`Repository::edit_tree`].
#[napi(object)]
pub struct TreeEditInput {
    /// Path of the entry to upsert or remove (e.g. `a/b/c.txt`).
    pub path: String,
    /// Mode for upserts (`100644`, `100755`, `040000`, `120000`, `160000`). Defaults to `100644`.
    pub mode: Option<String>,
    /// Object id for upserts. Required unless `remove` is set.
    pub id: Option<String>,
    /// When true, remove the entry at `path` instead of upserting.
    pub remove: Option<bool>,
}

/// Input for writing an annotated tag object.
#[derive(Clone)]
#[napi(object)]
pub struct TagInput {
    pub name: String,
    pub target: String,
    pub target_kind: String,
    pub message: String,
    pub tagger: Option<Signature>,
}

fn kind_to_string(kind: gix::object::Kind) -> String {
    match kind {
        gix::object::Kind::Commit => "commit",
        gix::object::Kind::Tree => "tree",
        gix::object::Kind::Blob => "blob",
        gix::object::Kind::Tag => "tag",
    }
    .to_string()
}

fn string_to_kind(s: &str) -> Result<gix::object::Kind> {
    match s.to_lowercase().as_str() {
        "commit" => Ok(gix::object::Kind::Commit),
        "tree" => Ok(gix::object::Kind::Tree),
        "blob" => Ok(gix::object::Kind::Blob),
        "tag" => Ok(gix::object::Kind::Tag),
        other => Err(err("write_object", format!("invalid object kind {other}"))),
    }
}

fn mode_to_entry_kind(mode: &str) -> Result<gix::objs::tree::EntryKind> {
    match mode {
        "100755" => Ok(gix::objs::tree::EntryKind::BlobExecutable),
        "040000" | "40000" => Ok(gix::objs::tree::EntryKind::Tree),
        "120000" => Ok(gix::objs::tree::EntryKind::Link),
        "160000" => Ok(gix::objs::tree::EntryKind::Commit),
        _ => Ok(gix::objs::tree::EntryKind::Blob),
    }
}

#[napi]
impl Repository {
    /// Find and decode a tree by id.
    #[napi]
    pub fn find_tree(&self, id: String) -> Result<Tree> {
        let r = thread_local(&self.inner);
        let tree = r
            .find_tree(parse_oid(&id)?)
            .map_err(|e| err("find_tree", e))?;
        build_tree(&self.inner, tree.id)
    }

    /// Find a blob by id (content is read lazily via [`Blob::content`]).
    #[napi]
    pub fn find_blob(&self, id: String) -> Result<Blob> {
        let id = parse_oid(&id)?;
        Ok(build_blob(&self.inner, id))
    }

    /// Find and decode a tag by id.
    #[napi]
    pub fn find_tag(&self, id: String) -> Result<Tag> {
        let r = thread_local(&self.inner);
        let tag = r
            .find_tag(parse_oid(&id)?)
            .map_err(|e| err("find_tag", e))?;
        build_tag(&self.inner, tag)
    }

    /// Find an object of any kind, returning a wrapper with `as_commit()` / `as_tree()` / etc.
    #[napi]
    pub fn find_object(&self, id: String) -> Result<GitObject> {
        let r = thread_local(&self.inner);
        let object = r
            .find_object(parse_oid(&id)?)
            .map_err(|e| err("find_object", e))?;
        build_object(&self.inner, object)
    }

    /// Whether an object with the given id exists.
    #[napi]
    pub fn object_exists(&self, id: String) -> bool {
        let r = thread_local(&self.inner);
        match parse_oid(&id) {
            Ok(oid) => r.has_object(oid),
            Err(_) => false,
        }
    }

    /// Write raw bytes as a blob and return its id.
    #[napi]
    pub fn write_blob(&self, data: Buffer) -> Result<String> {
        let r = thread_local(&self.inner);
        let id = r
            .write_blob(data.to_vec())
            .map_err(|e| err("write_blob", e))?;
        Ok(id.to_string())
    }

    /// Write an annotated tag object directly to the object database.
    #[napi]
    pub fn write_tag(&self, input: TagInput) -> Result<String> {
        let r = thread_local(&self.inner);
        let target = parse_oid(&input.target)?;
        let target_kind = match input.target_kind.to_lowercase().as_str() {
            "commit" => gix::objs::Kind::Commit,
            "tree" => gix::objs::Kind::Tree,
            "blob" => gix::objs::Kind::Blob,
            "tag" => gix::objs::Kind::Tag,
            _ => {
                return Err(err(
                    "write_tag",
                    format!("invalid target_kind {}", input.target_kind),
                ));
            }
        };

        let default_tagger = r.committer().and_then(|res| res.ok()).map(|s| s.into());
        let tagger = match input.tagger {
            Some(s) => Some(signature_from_dto(&s)?),
            None => default_tagger,
        };

        let tag = gix::objs::Tag {
            target,
            target_kind,
            name: input.name.into(),
            tagger,
            message: input.message.into(),
            signature: None,
        };

        let oid = r.write_object(&tag).map_err(|e| err("write_tag", e))?;
        Ok(oid.to_string())
    }

    /// Peel an object to a target object kind (`commit`, `tree`, `blob`, `tag`).
    #[napi]
    pub fn peel(&self, id: String, kind: String) -> Result<String> {
        let r = thread_local(&self.inner);
        let oid = parse_oid(&id)?;
        let target_kind = match kind.to_lowercase().as_str() {
            "commit" => gix::objs::Kind::Commit,
            "tree" => gix::objs::Kind::Tree,
            "blob" => gix::objs::Kind::Blob,
            "tag" => gix::objs::Kind::Tag,
            _ => return Err(err("peel", format!("invalid target kind {kind}"))),
        };
        let obj = r.find_object(oid).map_err(|e| err("find_object", e))?;
        let peeled = obj.peel_to_kind(target_kind).map_err(|e| err("peel", e))?;
        Ok(peeled.id().to_string())
    }

    /// Write a tree object constructed from tree item inputs and return its OID.
    #[napi]
    pub fn write_tree(&self, entries: Vec<TreeItemInput>) -> Result<String> {
        let r = thread_local(&self.inner);
        let mut tree_entries = Vec::new();
        for e in entries {
            let mode = match e.mode.as_deref().unwrap_or("100644") {
                "100755" => gix::objs::tree::EntryKind::BlobExecutable.into(),
                "040000" | "40000" => gix::objs::tree::EntryKind::Tree.into(),
                "120000" => gix::objs::tree::EntryKind::Link.into(),
                "160000" => gix::objs::tree::EntryKind::Commit.into(),
                _ => gix::objs::tree::EntryKind::Blob.into(),
            };
            tree_entries.push(gix::objs::tree::Entry {
                mode,
                filename: e.path.into(),
                oid: parse_oid(&e.id)?,
            });
        }
        let tree = gix::objs::Tree {
            entries: tree_entries,
        };
        let oid = r.write_object(&tree).map_err(|e| err("write_tree", e))?;
        Ok(oid.to_string())
    }

    /// Try to find an object by id, returning null if missing instead of throwing.
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

    /// Write a raw object of the given `kind` (`commit`, `tree`, `blob`, `tag`) and return its id.
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

    /// Write a blob using the streaming write path, returning its id.
    #[napi]
    pub fn write_blob_stream(&self, data: Buffer) -> Result<String> {
        let r = thread_local(&self.inner);
        let data = data.to_vec();
        let mut buf = data.as_slice();
        let id = r
            .write_stream(gix::object::Kind::Blob, data.len() as u64, &mut buf)
            .map_err(|e| err("write_blob_stream", e))?;
        Ok(id.to_string())
    }

    /// Edit a tree by applying `edits` (upsert/remove entries) on top of `base_tree_id`,
    /// returning the new tree id.
    #[napi]
    pub fn edit_tree(&self, base_tree_id: String, edits: Vec<TreeEditInput>) -> Result<String> {
        let r = thread_local(&self.inner);
        let base = parse_oid(&base_tree_id)?;
        let mut editor = r.edit_tree(base).map_err(|e| err("edit_tree", e))?;
        for e in &edits {
            if e.remove.unwrap_or(false) {
                editor
                    .remove(e.path.as_str())
                    .map_err(|e| err("edit_tree.remove", e))?;
            } else {
                let kind = mode_to_entry_kind(e.mode.as_deref().unwrap_or("100644"))?;
                let oid = parse_oid(
                    e.id.as_deref()
                        .ok_or_else(|| err("edit_tree", "upsert edit requires an `id`"))?,
                )?;
                editor
                    .upsert(e.path.as_str(), kind, oid)
                    .map_err(|e| err("edit_tree.upsert", e))?;
            }
        }
        let new_id = editor
            .write()
            .map_err(|e| err("edit_tree.write", e))?
            .detach();
        Ok(new_id.to_string())
    }

    /// Return the well-known empty tree object id as a [`Tree`].
    #[napi]
    pub fn empty_tree(&self) -> Result<Tree> {
        let r = thread_local(&self.inner);
        let id = gix::ObjectId::empty_tree(r.object_hash());
        build_tree(&self.inner, id)
    }

    /// Return the well-known empty blob object id as a [`Blob`].
    #[napi]
    pub fn empty_blob(&self) -> Result<Blob> {
        let r = thread_local(&self.inner);
        let id = r.object_hash().empty_blob();
        Ok(build_blob(&self.inner, id))
    }

    /// Read an object's header (kind + size) without decoding its full content.
    #[napi]
    pub fn find_header(&self, id: String) -> Result<ObjectHeader> {
        let r = thread_local(&self.inner);
        let header = r
            .find_header(parse_oid(&id)?)
            .map_err(|e| err("find_header", e))?;
        Ok(ObjectHeader {
            kind: kind_to_string(header.kind()),
            size: header.size() as f64,
        })
    }

    /// Like [`find_header`](Self::find_header) but returns `null` if the object is missing.
    #[napi]
    pub fn try_find_header(&self, id: String) -> Result<Option<ObjectHeader>> {
        let r = thread_local(&self.inner);
        match parse_oid(&id) {
            Ok(oid) => match r.find_header(oid) {
                Ok(header) => Ok(Some(ObjectHeader {
                    kind: kind_to_string(header.kind()),
                    size: header.size() as f64,
                })),
                Err(_) => Ok(None),
            },
            Err(_) => Ok(None),
        }
    }

    /// Set the in-memory object cache size (in bytes). Pass `0` to disable.
    ///
    /// Note: each synchronous call gets a fresh thread-local repository view, so this only
    /// affects the cache of the view used within this single call.
    #[napi]
    pub fn object_cache_size(&self, bytes: f64) -> Result<()> {
        let mut r = thread_local(&self.inner);
        r.object_cache_size(bytes as usize);
        Ok(())
    }

    /// Set the object cache size only if it has not been set already.
    ///
    /// See [`object_cache_size`](Self::object_cache_size) for the scoping caveat.
    #[napi]
    pub fn object_cache_size_if_unset(&self, bytes: f64) -> Result<()> {
        let mut r = thread_local(&self.inner);
        r.object_cache_size_if_unset(bytes as usize);
        Ok(())
    }

    /// Compute a recommended object cache size (in bytes) for tree-diff operations,
    /// based on the number of tracked files in the index.
    #[napi]
    pub fn compute_object_cache_size_for_tree_diffs(&self) -> Result<f64> {
        let r = thread_local(&self.inner);
        let index = r.index().map_err(|e| err("index", e))?;
        Ok(r.compute_object_cache_size_for_tree_diffs(&*index) as f64)
    }

    /// Enable keeping decompressed objects in memory for the lifetime of this view.
    ///
    /// See [`object_cache_size`](Self::object_cache_size) for the scoping caveat.
    #[napi]
    pub fn with_object_memory(&self) -> Result<()> {
        let r = thread_local(&self.inner);
        r.with_object_memory();
        Ok(())
    }
}
