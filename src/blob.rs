use std::sync::Arc;

use crate::error::RepositoryResult;
use gix::objs::Write;
use napi::bindgen_prelude::{Buffer, Either};
use napi_derive::napi;

use crate::error::{err, err_str, parse_oid, thread_local};
use crate::object::{Object, build_object};
use crate::repository::Repository;

#[napi]
pub struct Blob {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) id: gix::ObjectId,
}

#[napi]
impl Blob {
    #[napi(constructor)]
    pub fn new(repo: Option<&Repository>, content: Either<String, Buffer>) -> napi::Result<Self> {
        let inner = match repo {
            Some(r) => Arc::clone(&r.inner),
            None => crate::repository::current_repo()?,
        };
        let r = thread_local(&inner);
        let data: Vec<u8> = match content {
            Either::A(s) => s.into_bytes(),
            Either::B(b) => b.to_vec(),
        };
        let id = r.write_blob(data).map_err(|e| err("write_blob", e))?;
        Ok(Blob {
            repo: inner,
            id: id.detach(),
        })
    }

    /// Raw bytes of the blob.
    #[napi]
    pub fn content(&self) -> napi::Result<Buffer> {
        Ok(Buffer::from(self.content_bytes()?))
    }

    /// The blob's content as owned bytes, honoring any `replace` ref override.
    fn content_bytes(&self) -> napi::Result<Vec<u8>> {
        let r = thread_local(&self.repo);
        if let Some(replaced) = crate::replace::resolve_replace_head(&self.repo, &self.id) {
            let obj = r
                .find_object(replaced)
                .map_err(|e| err("find_object", e))?
                .detach();
            return Ok(obj.data);
        }
        let blob = r.find_blob(self.id).map_err(|e| err("find_blob", e))?;
        Ok(blob.data.to_vec())
    }

    /// True when the blob's first 8000 bytes contain a NUL byte — git's heuristic
    /// for classifying content as binary (matches libgit2's `git_blob_is_binary`).
    #[napi]
    pub fn is_binary(&self) -> napi::Result<bool> {
        let bytes = self.content_bytes()?;
        let scan = bytes.len().min(8000);
        Ok(bytes[..scan].contains(&0))
    }

    /// The byte length of the blob content.
    #[napi(getter)]
    pub fn size(&self) -> napi::Result<u32> {
        Ok(self.content_bytes()?.len() as u32)
    }

    /// View this blob as a generic [`Object`], enabling header/peel/`replace` access.
    #[napi(ts_return_type = "Object")]
    pub fn as_object(&self) -> napi::Result<Object> {
        build_object(&self.repo, self.id)
    }

    /// The object id of this blob.
    #[napi(getter)]
    pub fn id(&self) -> String {
        self.id.to_string()
    }
}

/// Construct a [`Blob`] wrapper from a repository and object id (used by `Repository` / `GitObject`).
pub fn build_blob(repo: &Arc<gix::ThreadSafeRepository>, id: gix::ObjectId) -> Blob {
    Blob {
        repo: Arc::clone(repo),
        id,
    }
}

#[napi]
impl Repository {
    /// Find a blob by id (content is read lazily via [`Blob::content`]).
    #[napi]
    pub fn find_blob(&self, id: String) -> napi::Result<Blob> {
        let id = parse_oid(&id)?;
        Ok(build_blob(&self.inner, id))
    }

    /// Write raw bytes as a blob and return its id.
    #[napi]
    pub fn write_blob(&self, data: Buffer) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        let id = r
            .write_blob(data.to_vec())
            .map_err(|e| err("write_blob", e))?;
        Ok(id.to_string())
    }

    /// Write a blob using the streaming write path, returning its id.
    #[napi]
    pub fn write_blob_stream(&self, data: Buffer) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        let data = data.to_vec();
        let mut buf = data.as_slice();
        let id = r
            .write_stream(gix::object::Kind::Blob, data.len() as u64, &mut buf)
            .map_err(|e| err("write_blob_stream", e))?;
        Ok(id.to_string())
    }

    /// Return the well-known empty blob object id as a [`Blob`].
    #[napi]
    pub fn empty_blob(&self) -> napi::Result<Blob> {
        let r = thread_local(&self.inner);
        let id = r.object_hash().empty_blob();
        Ok(build_blob(&self.inner, id))
    }

    /// Read `path` from disk and write its contents as a blob, returning the new id
    /// (mirrors git2's `Repository::blob_path`).
    #[napi]
    pub fn write_blob_path(&self, path: String) -> napi::Result<String> {
        let data = std::fs::read(&path).map_err(|e| err("read", e))?;
        let r = thread_local(&self.inner);
        let id = r.write_blob(data).map_err(|e| err("write_blob", e))?;
        Ok(id.to_string())
    }

    /// Resolve `refspec` (a branch name, `"HEAD"`, or a commit/tag/tree id) to a tree and
    /// return the blob stored at `path` within it — git's `<refspec>:<path>`
    /// (e.g. `git show HEAD:Cargo.toml`).
    ///
    /// Throws when the ref cannot be resolved, the path is missing, or the entry at `path`
    /// is a tree or submodule.
    #[napi]
    pub fn get_blob(&self, refspec: String, path: String) -> napi::Result<Blob> {
        let r = thread_local(&self.inner);
        let tree = resolve_tree(&r, &refspec)?;
        let blob_id = blob_at_path(&tree, &path)?;
        Ok(build_blob(&self.inner, blob_id))
    }

    /// Follow `branch`'s first-parent history to its root commit (the one with no parents)
    /// and return the blob at `path` in that commit's tree. Mirrors the snippet's `Blob::Init`.
    ///
    /// Throws when the branch cannot be resolved, the path is missing, or the entry at
    /// `path` is a tree or submodule.
    #[napi]
    pub fn get_blob_at_root(&self, branch: String, path: String) -> napi::Result<Blob> {
        let r = thread_local(&self.inner);
        let mut oid = match r.find_reference(&branch) {
            Ok(mut reference) => reference
                .peel_to_commit()
                .map_err(|e| err("peel_to_commit", e))?
                .id()
                .detach(),
            Err(_) => parse_oid(&branch)?,
        };
        loop {
            let commit = r.find_commit(oid).map_err(|e| err("find_commit", e))?;
            match commit.parent_ids().next() {
                None => break,
                Some(p) => oid = p.detach(),
            }
        }
        let tree = r
            .find_commit(oid)
            .map_err(|e| err("find_commit", e))?
            .tree()
            .map_err(|e| err("tree", e))?;
        let blob_id = blob_at_path(&tree, &path)?;
        Ok(build_blob(&self.inner, blob_id))
    }
}

/// Resolve `spec` to a tree: try it as a reference (branch / `"HEAD"` / tag), then fall back
/// to an object id (commit / tag / tree) peeled to its tree.
fn resolve_tree<'a>(r: &'a gix::Repository, spec: &str) -> napi::Result<gix::Tree<'a>> {
    if let Ok(mut reference) = r.find_reference(spec) {
        return reference.peel_to_tree().map_err(|e| err("peel_to_tree", e));
    }
    let oid = parse_oid(spec)?;
    let obj = r.find_object(oid).map_err(|e| err("find_object", e))?;
    obj.peel_to_tree().map_err(|e| err("peel_to_tree", e))
}

/// Look up `path` within `tree` and return the blob id at that location.
fn blob_at_path(tree: &gix::Tree<'_>, path: &str) -> napi::Result<gix::ObjectId> {
    let entry = tree
        .lookup_entry_by_path(std::path::Path::new(path))
        .map_err(|e| err("lookup_entry_by_path", e))?;
    let entry = match entry {
        Some(e) => e,
        None => return Err(err_str("get_blob", &format!("no such path: {path}"))),
    };
    let kind: gix::object::tree::EntryKind = entry.mode().into();
    match kind {
        gix::object::tree::EntryKind::Tree => {
            return Err(err_str(
                "get_blob",
                &format!("path is a tree, not a blob: {path}"),
            ));
        }
        gix::object::tree::EntryKind::Commit => {
            return Err(err_str(
                "get_blob",
                &format!("path is a submodule/gitlink, not a blob: {path}"),
            ));
        }
        _ => {}
    }
    Ok(entry.id().detach())
}
