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
use crate::tree::{Tree, build_tree};

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
        let id = crate::replace::resolve_replace_head(&self.repo, &self.id)
            .unwrap_or(self.id);
        let obj = r
            .find_object(id)
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

// ---- builders ---------------------------------------------------------------

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
    /// The object kind (`"blob"`, `"tree"`, `"commit"`, or `"tag"`).
    pub kind: String,
    /// The size of the object's encoded data in bytes.
    pub size: f64,
    /// `true` if the object was located in a pack (vs. a loose object).
    pub is_packed: bool,
    /// Number of deltas decoded to obtain this header; `null` for loose objects, always present for packed ones.
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

#[napi]
impl Repository {
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
            _ => return Err(err_str("peel", format!("invalid target kind {kind}"))),
        };
        let obj = r.find_object(oid).map_err(|e| err("find_object", e))?;
        let peeled = obj.peel_to_kind(target_kind).map_err(|e| err("peel", e))?;
        Ok(peeled.id().to_string())
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

    /// Read an object's kind and raw, decompressed body (the inverse of [`write_object`](Self::write_object)).
    #[napi]
    pub fn read_object(&self, id: String) -> Result<ObjectData> {
        let r = thread_local(&self.inner);
        let oid = parse_oid(&id)?;
        let resolved = crate::replace::resolve_replace_head(&self.inner, &oid).unwrap_or(oid);
        let obj = r
            .find_object(resolved)
            .map_err(|e| err("find_object", e))?
            .detach();
        Ok(ObjectData {
            kind: kind_to_string(obj.kind),
            data: Buffer::from(obj.data),
        })
    }

    /// Compute the object id of `data` of the given `kind` (`commit`, `tree`, `blob`, `tag`)
    /// without writing it to the object database.
    #[napi]
    pub fn compute_hash(&self, kind: String, data: Buffer) -> Result<String> {
        let r = thread_local(&self.inner);
        let kind = string_to_kind(&kind)?;
        let id = gix::objs::compute_hash(r.object_hash(), kind, &data)
            .map_err(|e| err("compute_hash", e))?;
        Ok(id.to_string())
    }

    /// Read an object's header (kind + size) without decoding its full content.
    ///
    /// Note: the repo-level header does not distinguish loose vs. packed objects, so `is_packed` is
    /// always `false` and `num_deltas` is always `null` here. For the pack-aware distinction, use
    /// [`ObjectHandle::try_header`]/[`ObjectDatabase::try_header`].
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

    /// Like [`find_header`](Self::find_header) but returns `null` if the object is missing.
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
