use std::sync::Arc;

use napi::bindgen_prelude::{Buffer, Error as NapiError};
use napi::Result;
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};

/// A git signature (author / committer / tagger), copied out as owned data.
#[derive(Clone)]
#[napi(object)]
pub struct Signature {
    pub name: String,
    pub email: String,
    /// Unix timestamp in seconds.
    pub time: i64,
    /// Timezone offset from UTC in minutes (east of UTC is positive).
    pub timezone_offset: i32,
}

fn signature_to_dto(s: &gix::actor::SignatureRef) -> Signature {
    let (time, timezone_offset) = s
        .time()
        .ok()
        .map(|t| (t.seconds, t.offset))
        .unwrap_or((0, 0));
    Signature {
        name: lossy(s.name),
        email: lossy(s.email),
        time,
        timezone_offset,
    }
}

fn lossy(b: &gix::bstr::BStr) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A decoded commit object, holding owned data plus a back-link to its repository.
#[napi]
pub struct Commit {
    #[napi(readonly)]
    pub id: String,
    #[napi(readonly)]
    pub message: String,
    #[napi(readonly)]
    pub tree_id: String,
    #[napi(readonly)]
    pub parent_ids: Vec<String>,
    author: Signature,
    committer: Signature,
    repo: Arc<gix::ThreadSafeRepository>,
}

#[napi]
impl Commit {
    /// The author signature of this commit.
    #[napi(getter)]
    pub fn author(&self) -> Signature {
        self.author.clone()
    }

    /// The committer signature of this commit.
    #[napi(getter)]
    pub fn committer(&self) -> Signature {
        self.committer.clone()
    }

    /// The parent commits of this commit, re-derived from the repository.
    #[napi]
    pub fn parents(&self) -> Result<Vec<Commit>> {
        let repo = thread_local(&self.repo);
        let mut out = Vec::with_capacity(self.parent_ids.len());
        for p in &self.parent_ids {
            let id = parse_oid(p)?;
            let commit = repo.find_commit(id).map_err(|e| err("find_commit", e))?;
            out.push(build_commit(&self.repo, commit)?);
        }
        Ok(out)
    }

    /// The tree object this commit points to.
    #[napi]
    pub fn tree(&self) -> Result<Tree> {
        build_tree(&self.repo, parse_oid(&self.tree_id)?)
    }
}

/// Build a [`Commit`] from a found, decoded `gix::Commit`.
pub(crate) fn build_commit(
    repo: &Arc<gix::ThreadSafeRepository>,
    commit: gix::Commit,
) -> Result<Commit> {
    let id = commit.id;
    let c = commit.decode().map_err(|e| err("decode commit", e))?;
    let author = c.author().map_err(|e| err("author", e))?;
    let committer = c.committer().map_err(|e| err("committer", e))?;
    Ok(Commit {
        id: id.to_string(),
        message: lossy(c.message),
        author: signature_to_dto(&author),
        committer: signature_to_dto(&committer),
        tree_id: c.tree().to_string(),
        parent_ids: c.parents().map(|p| p.to_string()).collect(),
        repo: Arc::clone(repo),
    })
}

/// A single entry inside a tree.
#[derive(Clone)]
#[napi(object)]
pub struct TreeEntry {
    pub name: String,
    pub id: String,
    /// Git mode as a string, e.g. `"100644"`, `"100755"`, `"040000"`, `"120000"`, `"160000"`.
    pub mode: String,
}

/// A decoded tree object.
#[napi]
pub struct Tree {
    #[napi(readonly)]
    pub id: String,
    entries: Vec<TreeEntry>,
    repo: Arc<gix::ThreadSafeRepository>,
}

#[napi]
impl Tree {
    /// All entries of this tree.
    #[napi(getter)]
    pub fn entries(&self) -> Vec<TreeEntry> {
        self.entries.clone()
    }

    /// Look up a direct child entry by its file/dir name.
    #[napi]
    pub fn entry(&self, name: String) -> Option<TreeEntry> {
        self.entries.iter().find(|e| e.name == name).cloned()
    }
}

/// Build a [`Tree`] from a found, decoded `gix::Tree`.
pub(crate) fn build_tree(
    repo: &Arc<gix::ThreadSafeRepository>,
    id: impl Into<gix::ObjectId>,
) -> Result<Tree> {
    let id = id.into();
    let r = thread_local(repo);
    let tree = r.find_tree(id).map_err(|e| err("find_tree", e))?;
    let id = tree.id;
    let t = tree.decode().map_err(|e| err("decode tree", e))?;
    let entries = t
        .entries
        .iter()
        .map(|e| TreeEntry {
            name: lossy(e.filename),
            id: e.oid.to_string(),
            mode: format!("{:?}", e.mode.kind()),
        })
        .collect();
    Ok(Tree {
        id: id.to_string(),
        entries,
        repo: Arc::clone(repo),
    })
}

/// A decoded blob object.
#[napi]
pub struct Blob {
    #[napi(readonly)]
    pub id: String,
    repo: Arc<gix::ThreadSafeRepository>,
}

#[napi]
impl Blob {
    /// The raw bytes of the blob.
    #[napi]
    pub fn content(&self) -> Result<Buffer> {
        let repo = thread_local(&self.repo);
        let id = parse_oid(&self.id)?;
        let blob = repo.find_blob(id).map_err(|e| err("find_blob", e))?;
        Ok(Buffer::from(blob.data.clone()))
    }
}

/// Build a [`Blob`] wrapper (data is fetched lazily via [`Blob::content`]).
pub(crate) fn build_blob(repo: &Arc<gix::ThreadSafeRepository>, id: gix::ObjectId) -> Blob {
    Blob {
        id: id.to_string(),
        repo: Arc::clone(repo),
    }
}

/// A decoded (annotated) tag object.
#[napi]
pub struct Tag {
    #[napi(readonly)]
    pub id: String,
    #[napi(readonly)]
    pub name: String,
    #[napi(readonly)]
    pub target_id: String,
    /// The kind of object the tag points at: `Commit`, `Tree`, `Blob`, or `Tag`.
    #[napi(readonly)]
    pub target_kind: String,
    tagger: Option<Signature>,
    #[napi(readonly)]
    pub message: String,
    repo: Arc<gix::ThreadSafeRepository>,
}

#[napi]
impl Tag {
    /// The tagger signature, if this is an annotated tag.
    #[napi(getter)]
    pub fn tagger(&self) -> Option<Signature> {
        self.tagger.clone()
    }
}

/// Build a [`Tag`] from a found, decoded `gix::Tag`.
pub(crate) fn build_tag(repo: &Arc<gix::ThreadSafeRepository>, tag: gix::Tag) -> Result<Tag> {
    let id = tag.id;
    let t = tag.decode().map_err(|e| err("decode tag", e))?;
    let target_id = t.target();
    let tagger = t
        .tagger()
        .map_err(|e| err("tagger", e))?
        .map(|s| signature_to_dto(&s));
    Ok(Tag {
        id: id.to_string(),
        name: lossy(t.name),
        target_id: target_id.to_string(),
        target_kind: format!("{:?}", t.target_kind),
        tagger,
        message: lossy(t.message),
        repo: Arc::clone(repo),
    })
}

/// A git object of any kind, with downcast accessors to the rich typed classes.
///
/// napi-rs does not support tagged unions, so `findObject` returns this wrapper and you call
/// `as_commit()` / `as_tree()` / `as_blob()` / `as_tag()` (whichever matches `kind`).
#[napi]
pub struct GitObject {
    #[napi(readonly)]
    pub id: String,
    /// One of `commit`, `tree`, `blob`, `tag`.
    #[napi(readonly)]
    pub kind: String,
    repo: Arc<gix::ThreadSafeRepository>,
}

#[napi]
impl GitObject {
    #[napi]
    pub fn as_commit(&self) -> Result<Option<Commit>> {
        if self.kind != "commit" {
            return Ok(None);
        }
        let repo = thread_local(&self.repo);
        let commit = repo
            .find_commit(parse_oid(&self.id)?)
            .map_err(|e| err("find_commit", e))?;
        Ok(Some(build_commit(&self.repo, commit)?))
    }

    #[napi]
    pub fn as_tree(&self) -> Result<Option<Tree>> {
        if self.kind != "tree" {
            return Ok(None);
        }
        Ok(Some(build_tree(&self.repo, parse_oid(&self.id)?)?))
    }

    #[napi]
    pub fn as_blob(&self) -> Result<Option<Blob>> {
        if self.kind != "blob" {
            return Ok(None);
        }
        Ok(Some(build_blob(&self.repo, parse_oid(&self.id)?)))
    }

    #[napi]
    pub fn as_tag(&self) -> Result<Option<Tag>> {
        if self.kind != "tag" {
            return Ok(None);
        }
        let repo = thread_local(&self.repo);
        let tag = repo
            .find_tag(parse_oid(&self.id)?)
            .map_err(|e| err("find_tag", e))?;
        Ok(Some(build_tag(&self.repo, tag)?))
    }
}

/// Helper used by the repository layer to materialise a [`GitObject`] from a found object.
pub(crate) fn build_object(
    repo: &Arc<gix::ThreadSafeRepository>,
    object: gix::Object,
) -> Result<GitObject> {
    let id = object.id().to_string();
    let kind = format!("{:?}", object.kind);
    Ok(GitObject {
        id,
        kind: kind.to_lowercase(),
        repo: Arc::clone(repo),
    })
}

#[allow(dead_code)]
fn _assert_send() {
    fn is_send<T: Send>() {}
    is_send::<Commit>();
    is_send::<Tree>();
    is_send::<Blob>();
    is_send::<Tag>();
    is_send::<NapiError>();
}
