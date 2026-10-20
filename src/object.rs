use std::sync::Arc;

use gix::bstr::ByteSlice;
use napi::bindgen_prelude::Buffer;
use napi::Result;
use napi_derive::napi;

use crate::error::{err, thread_local};

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
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
}

/// A git commit exposed to TypeScript.
#[napi]
pub struct Commit {
    repo: Arc<gix::ThreadSafeRepository>,
    id: gix::ObjectId,
}

#[napi]
impl Commit {
    #[napi(getter)]
    pub fn id(&self) -> String {
        self.id.to_string()
    }

    /// Full commit message.
    #[napi(getter)]
    pub fn message(&self) -> Result<String> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let commit = commit.decode().map_err(|e| err("decode", e))?;
        Ok(lossy(commit.message))
    }

    /// The root tree id of the commit.
    #[napi(getter)]
    pub fn tree_id(&self) -> Result<String> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let commit = commit.decode().map_err(|e| err("decode", e))?;
        Ok(commit.tree.to_string())
    }

    /// Parent commit ids.
    #[napi(getter)]
    pub fn parent_ids(&self) -> Result<Vec<String>> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        let commit = commit.decode().map_err(|e| err("decode", e))?;
        Ok(commit.parents().map(|p| p.to_string()).collect())
    }

    /// Check whether the commit has a signature (e.g. gpgsig or gpgsig-sha256).
    #[napi(getter)]
    pub fn is_signed(&self) -> Result<bool> {
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        Ok(commit.signature().map_err(|e| err("signature", e))?.is_some())
    }
}

/// A single entry of a git tree.
#[napi(object)]
pub struct TreeEntry {
    pub mode: String,
    pub id: String,
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

    #[napi]
    pub fn as_commit(&self) -> Result<Option<Commit>> {
        if self.kind != "commit" {
            return Ok(None);
        }
        let r = thread_local(&self.repo);
        let commit = r.find_commit(self.id).map_err(|e| err("find_commit", e))?;
        Ok(Some(build_commit(&self.repo, commit)?))
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
}

// ---- builders ---------------------------------------------------------------

pub fn build_blob(repo: &Arc<gix::ThreadSafeRepository>, id: gix::ObjectId) -> Blob {
    Blob {
        repo: Arc::clone(repo),
        id,
    }
}

pub fn build_commit(
    repo: &Arc<gix::ThreadSafeRepository>,
    commit: gix::Commit,
) -> Result<Commit> {
    Ok(Commit {
        repo: Arc::clone(repo),
        id: commit.id,
    })
}

pub fn build_tree(
    repo: &Arc<gix::ThreadSafeRepository>,
    id: gix::ObjectId,
) -> Result<Tree> {
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
