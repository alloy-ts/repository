use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use napi::Result;
use napi::Task;
use napi::bindgen_prelude::AsyncTask;
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};
use crate::object::{
    build_blob, build_commit, build_object, build_tag, build_tree, Blob, Commit, GitObject, Signature, Tag,
    Tree,
};
use crate::reference::{build_head, build_reference, Head, Reference};
use gix::prelude::Write;

use crate::remote::build_remote;
use crate::remote::Remote;

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[derive(Clone)]
#[napi(object)]
pub struct TreeItemInput {
    pub name: String,
    pub mode: String,
    pub id: String,
}

fn parse_tree_mode(m: &str) -> Result<gix::objs::tree::EntryMode> {
    let kind = match m {
        "Blob" => gix::objs::tree::EntryKind::Blob,
        "BlobExecutable" => gix::objs::tree::EntryKind::BlobExecutable,
        "Tree" => gix::objs::tree::EntryKind::Tree,
        "Link" => gix::objs::tree::EntryKind::Link,
        "Commit" => gix::objs::tree::EntryKind::Commit,
        _ => return Err(napi::Error::from_reason(format!("invalid tree entry mode: {m}"))),
    };
    Ok(kind.into())
}

#[derive(Clone)]
#[napi(object)]
pub struct SubmoduleInfo {
    pub name: String,
    pub path: String,
    pub url: Option<String>,
}

#[derive(Clone)]
#[napi(object)]
pub struct WorktreeInfo {
    pub name: Option<String>,
    pub git_dir: String,
    pub work_dir: Option<String>,
    pub is_main: bool,
    pub is_locked: bool,
}

#[derive(Clone)]
#[napi(object)]
pub struct MergeResult {
    pub tree_id: String,
    pub conflicts: Vec<String>,
    pub has_conflicts: bool,
}

#[derive(Clone)]
#[napi(object)]
pub struct ConfigEntry {
    pub key: String,
    pub value: String,
}

#[derive(Clone)]
#[napi(object)]
pub struct IndexEntry {
    pub path: String,
    pub mode: String,
    pub id: String,
    pub stage: i32,
    pub size: u32,
}

#[derive(Clone)]
#[napi(object)]
pub struct StatusEntry {
    pub path: String,
    pub status: String,
}

#[napi(object)]
pub struct RefLogEntry {
    /// The object id the ref pointed at *before* this update.
    pub old_id: String,
    /// The object id the ref points at *after* this update.
    pub new_id: String,
    /// Name of the actor who performed the update.
    pub name: String,
    /// Email of the actor who performed the update.
    pub email: String,
    /// Seconds since the Unix epoch when the update happened.
    pub time: i64,
    /// Timezone offset from UTC in minutes (east of UTC is positive).
    pub timezone_offset: i32,
    /// Free-form message describing the operation (e.g. `commit: ...`, `clone: from ...`).
    pub message: String,
}

/// Blocking clone work, executed on the libuv thread pool by [`AsyncTask`].
struct CloneTask {
    url: String,
    path: String,
    bare: bool,
}

impl Task for CloneTask {
    type Output = Repository;
    type JsValue = Repository;

    fn compute(&mut self) -> Result<Repository> {
        if std::env::var_os("GIT_COMMITTER_NAME").is_none() {
            unsafe { std::env::set_var("GIT_COMMITTER_NAME", "gitoxide") };
        }
        if std::env::var_os("GIT_COMMITTER_EMAIL").is_none() {
            unsafe { std::env::set_var("GIT_COMMITTER_EMAIL", "gitoxide@localhost") };
        }
        if std::env::var_os("GIT_AUTHOR_NAME").is_none() {
            unsafe { std::env::set_var("GIT_AUTHOR_NAME", "gitoxide") };
        }
        if std::env::var_os("GIT_AUTHOR_EMAIL").is_none() {
            unsafe { std::env::set_var("GIT_AUTHOR_EMAIL", "gitoxide@localhost") };
        }

        if is_local_url(&self.url) {
            let _ = std::fs::remove_dir_all(&self.path);
            let mut cmd = std::process::Command::new("git");
            cmd.arg("clone");
            if self.bare {
                cmd.arg("--bare");
            }
            cmd.arg(&self.url).arg(&self.path);
            return match cmd.status() {
                Ok(s) if s.success() => {
                    let repo = gix::open(&self.path)
                        .map_err(|e| napi::Error::from_reason(format!("open: {e}")))?;
                    Ok(Repository {
                        inner: Arc::new(repo.into()),
                    })
                }
                _ => Err(napi::Error::from_reason(format!(
                    "clone: git CLI local clone of {:?} failed",
                    self.url
                ))),
            };
        }

        let mut prepare = if self.bare {
            gix::prepare_clone_bare(self.url.clone(), self.path.as_str())
        } else {
            gix::prepare_clone(self.url.clone(), self.path.as_str())
        }
        .map_err(|e| napi::Error::from_reason(format!("prepare_clone: {e}")))?;
        let interrupt = AtomicBool::new(false);
        let (repo, _outcome) = prepare
            .fetch_only(gix::progress::Discard, &interrupt)
            .map_err(|e| napi::Error::from_reason(format!("fetch: {e}")))?;
        Ok(Repository {
            inner: Arc::new(repo.into()),
        })
    }

    fn resolve(&mut self, _env: napi::Env, output: Repository) -> Result<Repository> {
        Ok(output)
    }
}
fn is_local_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    !(lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("ssh://")
        || lower.starts_with("git://")
        || lower.starts_with("rsync://"))
}

/// The primary handle to a git repository, exposed to TypeScript as a class.
///
/// Internally wraps `Arc<gix::ThreadSafeRepository>` so it can be shared and offloaded to worker
/// threads for async network operations, while sync methods obtain a cheap thread-local view.
#[napi]
pub struct Repository {
    pub(crate) inner: Arc<gix::ThreadSafeRepository>,
}

#[napi]
impl Repository {
    // ---- construction (static factory methods) -------------------------

    /// Open the repository at `path` (expands `path/.git` for worktrees).
    #[napi]
    pub fn open(path: String) -> Result<Repository> {
        let ts = gix::ThreadSafeRepository::open(path)
            .map_err(|e| err("open", e))?;
        Ok(Repository {
            inner: Arc::new(ts),
        })
    }

    /// Initialize a repository at `path`. Pass `bare: true` for a bare repository.
    #[napi]
    pub fn init(path: String, bare: Option<bool>) -> Result<Repository> {
        let r = if bare.unwrap_or(false) {
            gix::init_bare(path)
        } else {
            gix::init(path)
        }
        .map_err(|e| err("init", e))?;
        Ok(Repository {
            inner: Arc::new(r.into()),
        })
    }

    /// Discover a repository by walking up from `path` (or the current directory if omitted).
    #[napi]
    pub fn discover(path: Option<String>) -> Result<Repository> {
        let p = path.unwrap_or_else(|| ".".to_string());
        let r = gix::discover(p).map_err(|e| err("discover", e))?;
        Ok(Repository {
            inner: Arc::new(r.into()),
        })
    }

    /// Clone a repository from `url` into `path`.
    ///
    /// Async: the (blocking) network transfer runs on a libuv worker thread and resolves a
    /// `Promise<Repository>`.
    #[napi(ts_return_type = "Promise<Repository>")]
    pub fn clone(url: String, path: String, bare: Option<bool>) -> Result<AsyncTask<CloneTask>> {
        Ok(AsyncTask::new(CloneTask {
            url,
            path,
            bare: bare.unwrap_or(false),
        }))
    }

    // ---- head / objects --------------------------------------------------

    /// The repository `HEAD`.
    #[napi]
    pub fn head(&self) -> Result<Head> {
        let r = thread_local(&self.inner);
        build_head(&self.inner, r.head().map_err(|e| err("head", e))?)
    }

    /// The commit `HEAD` points to.
    #[napi]
    pub fn head_commit(&self) -> Result<Commit> {
        let r = thread_local(&self.inner);
        build_commit(&self.inner, r.head_commit().map_err(|e| err("head_commit", e))?)
    }

    /// The object id `HEAD` points to.
    #[napi]
    pub fn head_id(&self) -> Result<String> {
        let r = thread_local(&self.inner);
        let id = r.head_id().map_err(|e| err("head_id", e))?;
        Ok(id.to_string())
    }

    /// The symbolic referent name of `HEAD`, if it is not detached.
    #[napi]
    pub fn head_name(&self) -> Result<Option<String>> {
        let r = thread_local(&self.inner);
        Ok(r.head_name().map_err(|e| err("head_name", e))?.map(|n| n.to_string()))
    }

    /// The tree id pointed to by `HEAD`.
    #[napi]
    pub fn head_tree_id(&self) -> Result<String> {
        let r = thread_local(&self.inner);
        let id = r.head_tree_id().map_err(|e| err("head_tree_id", e))?;
        Ok(id.to_string())
    }

    /// `HEAD` resolved to a `Reference`, or `null` if detached or unborn.
    #[napi]
    pub fn head_ref(&self) -> Result<Option<Reference>> {
        let r = thread_local(&self.inner);
        let head_ref = r.head_ref().map_err(|e| err("head_ref", e))?;
        Ok(head_ref.map(|rf| build_reference(&self.inner, rf)).transpose()?)
    }

    /// The `Tree` pointed to by `HEAD`.
    #[napi]
    pub fn head_tree(&self) -> Result<Tree> {
        let id = self.head_tree_id()?;
        self.find_tree(id)
    }

    /// Point `HEAD` to a ref name (e.g. `refs/heads/main`) or detach `HEAD` at a commit OID.
    #[napi]
    pub fn set_head(&self, target: String) -> Result<()> {
        let r = thread_local(&self.inner);
        if target.starts_with("refs/") {
            let name = gix::refs::FullName::try_from(target)
                .map_err(|e| err("invalid ref name", e))?;
            let edit = gix::refs::transaction::RefEdit {
                name: gix::refs::FullName::try_from("HEAD").map_err(|e| err("HEAD ref", e))?,
                change: gix::refs::transaction::Change::Update {
                    expected: gix::refs::transaction::PreviousValue::Any,
                    new: gix::refs::Target::Symbolic(name),
                    log: gix::refs::transaction::LogChange {
                        mode: gix::refs::transaction::RefLog::Only,
                        message: "set HEAD".into(),
                        force_create_reflog: false,
                    },
                },
                deref: false,
            };
            r.edit_reference(edit).map_err(|e| err("set_head", e))?;
        } else {
            let oid = parse_oid(&target)?;
            r.reference("HEAD", oid, gix::refs::transaction::PreviousValue::Any, "set HEAD detached")
                .map_err(|e| err("set_head_detached", e))?;
        }
        Ok(())
    }

    /// Find and decode a commit by id.
    #[napi]
    pub fn find_commit(&self, id: String) -> Result<Commit> {
        let r = thread_local(&self.inner);
        let commit = r
            .find_commit(parse_oid(&id)?)
            .map_err(|e| err("find_commit", e))?;
        build_commit(&self.inner, commit)
    }

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
        let tag = r.find_tag(parse_oid(&id)?).map_err(|e| err("find_tag", e))?;
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
    pub fn write_blob(&self, data: napi::bindgen_prelude::Buffer) -> Result<String> {
        let r = thread_local(&self.inner);
        let id = r
            .write_blob(data.to_vec())
            .map_err(|e| err("write_blob", e))?;
        Ok(id.to_string())
    }

    /// Write a raw object of `kind` ("commit", "tree", "blob", "tag") with `data` buffer into the repository.
    #[napi]
    pub fn write_object(&self, kind: String, data: napi::bindgen_prelude::Buffer) -> Result<String> {
        let r = thread_local(&self.inner);
        let k = match kind.to_ascii_lowercase().as_str() {
            "commit" => gix::objs::Kind::Commit,
            "tree" => gix::objs::Kind::Tree,
            "blob" => gix::objs::Kind::Blob,
            "tag" => gix::objs::Kind::Tag,
            _ => return Err(napi::Error::from_reason(format!("invalid object kind: {kind}"))),
        };
        let id = r.write_buf(k, &data).map_err(|e| err("write_object", e))?;
        Ok(id.to_string())
    }

    /// Construct and write a tree object from `entries`.
    #[napi]
    pub fn write_tree(&self, entries: Vec<TreeItemInput>) -> Result<String> {
        let r = thread_local(&self.inner);
        let mut tree = gix::objs::Tree::empty();
        for item in entries {
            tree.entries.push(gix::objs::tree::Entry {
                mode: parse_tree_mode(&item.mode)?,
                filename: item.name.into(),
                oid: parse_oid(&item.id)?,
            });
        }
        let id = r.write_object(&tree).map_err(|e| err("write_tree", e))?;
        Ok(id.to_string())
    }

    /// Try to find an object by ID, returning `null` if not found instead of throwing.
    #[napi]
    pub fn try_find_object(&self, id: String) -> Result<Option<GitObject>> {
        let r = thread_local(&self.inner);
        let oid = match parse_oid(&id) {
            Ok(o) => o,
            Err(_) => return Ok(None),
        };
        match r.try_find_object(oid).map_err(|e| err("try_find_object", e))? {
            Some(obj) => Ok(Some(build_object(&self.inner, obj)?)),
            None => Ok(None),
        }
    }

    // ---- references -------------------------------------------------------

    /// All references in the repository.
    #[napi]
    pub fn references(&self) -> Result<Vec<Reference>> {
        let r = thread_local(&self.inner);
        let platform = r.references().map_err(|e| err("references", e))?;
        let mut out = Vec::new();
        for reference in platform.all().map_err(|e| err("references iter", e))? {
            let reference = reference.map_err(|e| err("reference", e))?;
            out.push(build_reference(&self.inner, reference)?);
        }
        Ok(out)
    }

    /// Find a single reference by name (e.g. `main`, `refs/heads/main`, `HEAD`).
    #[napi]
    pub fn find_reference(&self, name: String) -> Result<Reference> {
        let r = thread_local(&self.inner);
        let reference = r
            .find_reference(name.as_str())
            .map_err(|e| err("find_reference", e))?;
        build_reference(&self.inner, reference)
    }

    /// Create (or, if `force`, overwrite) a reference pointing at `target`.
    #[napi]
    pub fn create_reference(
        &self,
        name: String,
        target: String,
        force: Option<bool>,
    ) -> Result<Reference> {
        let r = thread_local(&self.inner);
        let constraint = if force.unwrap_or(false) {
            gix::refs::transaction::PreviousValue::Any
        } else {
            gix::refs::transaction::PreviousValue::MustNotExist
        };
        let label = format!("create reference {name}");
        let reference = r
            .reference(name, parse_oid(&target)?, constraint, label)
            .map_err(|e| err("create_reference", e))?;
        build_reference(&self.inner, reference)
    }

    #[napi]
    pub fn reflog(&self, name: String) -> Result<Vec<RefLogEntry>> {
        let r = thread_local(&self.inner);
        let mut buf = [0u8; 8192];
        let iter = r
            .refs
            .reflog_iter_rev(name.as_str(), &mut buf)
            .map_err(|e| err("reflog_iter_rev", e))?;
        let Some(iter) = iter else {
            return Ok(Vec::new());
        };
        let mut out = Vec::with_capacity(64);
        for line in iter {
            let line = line.map_err(|e| err("reflog line", e))?;
            let t = line.signature.time;
            out.push(RefLogEntry {
                old_id: line.previous_oid.to_string(),
                new_id: line.new_oid.to_string(),
                name: line.signature.name.to_string(),
                email: line.signature.email.to_string(),
                time: t.seconds,
                timezone_offset: t.offset,
                message: line.message.to_string(),
            });
        }
        Ok(out)
    }

    /// Create an annotated tag `name` -> `target`.
    #[napi]
    pub fn create_tag(
        &self,
        name: String,
        target: String,
        message: String,
        force: Option<bool>,
    ) -> Result<Reference> {
        let r = thread_local(&self.inner);
        let target_id = parse_oid(&target)?;
        let kind = r
            .find_object(target_id.clone())
            .map_err(|e| err("find_object", e))?
            .kind;
        let tagger = r.committer().and_then(|res| res.ok());
        let constraint = if force.unwrap_or(false) {
            gix::refs::transaction::PreviousValue::Any
        } else {
            gix::refs::transaction::PreviousValue::MustNotExist
        };
        let reference = r
            .tag(name, target_id, kind, tagger, message.as_str(), constraint)
            .map_err(|e| err("create_tag", e))?;
        build_reference(&self.inner, reference)
    }

    /// Create a lightweight tag `name` -> `target`.
    #[napi]
    pub fn create_lightweight_tag(
        &self,
        name: String,
        target: String,
        force: Option<bool>,
    ) -> Result<Reference> {
        let r = thread_local(&self.inner);
        let constraint = if force.unwrap_or(false) {
            gix::refs::transaction::PreviousValue::Any
        } else {
            gix::refs::transaction::PreviousValue::MustNotExist
        };
        let reference = r
            .tag_reference(name, parse_oid(&target)?, constraint)
            .map_err(|e| err("create_lightweight_tag", e))?;
        build_reference(&self.inner, reference)
    }

    // ---- remotes ----------------------------------------------------------

    /// All configured (named) remotes.
    #[napi]
    pub fn remotes(&self) -> Result<Vec<Remote>> {
        let r = thread_local(&self.inner);
        let names = r.remote_names();
        let mut out = Vec::new();
        for name in names.iter() {
            if let Ok(remote) = r.find_remote(&**name) {
                out.push(build_remote(&self.inner, remote));
            }
        }
        Ok(out)
    }

    /// Look up a single named remote.
    #[napi]
    pub fn find_remote(&self, name: String) -> Result<Remote> {
        let mut r = thread_local(&self.inner);
        let _ = r.reload();
        let remote = r
            .find_remote(name.as_str())
            .map_err(|e| err("find_remote", e))?;
        Ok(build_remote(&self.inner, remote))
    }

    /// Create an anonymous remote targeting `url`.
    #[napi]
    pub fn remote_at(&self, url: String) -> Result<Remote> {
        let r = thread_local(&self.inner);
        let remote = r.remote_at(url.as_str()).map_err(|e| err("remote_at", e))?;
        Ok(build_remote(&self.inner, remote))
    }

    /// Create and persist a named remote with `url`.
    #[napi]
    pub fn create_remote(&self, name: String, url: String) -> Result<Remote> {
        let mut r = thread_local(&self.inner);
        let config_path = r
            .config_path(gix::config::Source::Local)
            .map_err(|e| err("config_path", e))?;
        let mut tx = r
            .config_file_mut(&config_path)
            .map_err(|e| err("config_file_mut", e))?;
        let mut remote = r
            .remote_at(url.as_str())
            .map_err(|e| err("remote_at", e))?
            .with_refspecs(
                [format!("+refs/heads/*:refs/remotes/{name}/*").as_str()],
                gix::remote::Direction::Fetch,
            )
            .map_err(|e| err("with_refspecs", e))?;
        remote
            .save_as_to(name.as_str(), &mut tx)
            .map_err(|e| err("save_as_to", e))?;
        tx.commit().map_err(|e| err("commit_config", e))?;
        r.reload().map_err(|e| err("reload", e))?;
        let remote = r
            .find_remote(&name)
            .map_err(|e| err("find_remote", e))?;
        Ok(build_remote(&self.inner, remote))
    }

    // ---- index & status --------------------------------------------------

    /// Read entries from the repository index (`.git/index`).
    #[napi]
    pub fn index(&self) -> Result<Vec<IndexEntry>> {
        let r = thread_local(&self.inner);
        let index = r.index().map_err(|e| err("index", e))?;
        let mut out = Vec::new();
        for entry in index.entries() {
            let p: &gix::bstr::BStr = entry.path(&index);
            out.push(IndexEntry {
                path: lossy(p),
                mode: format!("{:?}", entry.mode),
                id: entry.id.to_string(),
                stage: entry.stage() as i32,
                size: entry.stat.size,
            });
        }
        Ok(out)
    }

    /// Read entries from the repository index, loading from HEAD if needed.
    #[napi]
    pub fn index_or_load_from_head(&self) -> Result<Vec<IndexEntry>> {
        let r = thread_local(&self.inner);
        let index = r
            .index_or_load_from_head()
            .map_err(|e| err("index_or_load_from_head", e))?;
        let mut out = Vec::new();
        for entry in index.entries() {
            let p: &gix::bstr::BStr = entry.path(&index);
            out.push(IndexEntry {
                path: lossy(p),
                mode: format!("{:?}", entry.mode),
                id: entry.id.to_string(),
                stage: entry.stage() as i32,
                size: entry.stat.size,
            });
        }
        Ok(out)
    }

    /// Status of working directory files compared to index / HEAD.
    #[napi]
    pub fn status(&self) -> Result<Vec<StatusEntry>> {
        let r = thread_local(&self.inner);
        let status_platform = r.status(gix::progress::Discard).map_err(|e| err("status", e))?;
        let mut out = Vec::new();
        let iter = status_platform
            .into_index_worktree_iter(Vec::<gix::bstr::BString>::new())
            .map_err(|e| err("index_worktree_iter", e))?;
        for item in iter {
            let item = item.map_err(|e| err("status item", e))?;
            out.push(StatusEntry {
                path: lossy(item.rela_path()),
                status: format!("{item:?}"),
            });
        }
        Ok(out)
    }

    // ---- config / paths --------------------------------------------------

    /// Read a single config value as a string.
    #[napi]
    pub fn config_string(&self, key: String) -> Option<String> {
        let mut r = thread_local(&self.inner);
        let _ = r.reload();
        r.config_snapshot().string(key.as_str()).map(|v| lossy(&*v))
    }

    /// Set a config key to a string value in the repository's local config file.
    #[napi]
    pub fn set_config_string(&self, key: String, value: String) -> Result<()> {
        let mut r = thread_local(&self.inner);
        let config_path = r
            .config_path(gix::config::Source::Local)
            .map_err(|e| err("config_path", e))?;
        let mut tx = r
            .config_file_mut(&config_path)
            .map_err(|e| err("config_file_mut", e))?;
        let (section_name, k) = key.split_once('.').unwrap_or(("core", key.as_str()));
        let mut section = tx
            .section_mut_or_create_new(section_name, None::<&gix::bstr::BStr>)
            .map_err(|e| err("section_mut_or_create_new", e))?;
        section.set(k, value.as_str()).map_err(|e| err("set", e))?;
        tx.commit().map_err(|e| err("commit_config", e))?;
        let _ = r.reload();
        Ok(())
    }

    /// List all local branch names in the repository.
    #[napi]
    pub fn branch_names(&self) -> Result<Vec<String>> {
        let r = thread_local(&self.inner);
        let platform = r.references().map_err(|e| err("references", e))?;
        let branches = platform.local_branches().map_err(|e| err("local_branches", e))?;
        let mut out = Vec::new();
        for b in branches {
            let b = b.map_err(|e| err("branch ref", e))?;
            out.push(lossy(b.name().shorten()));
        }
        Ok(out)
    }

    /// The remote name configured for `branch` (e.g. `branch.main.remote`), if any.
    #[napi]
    pub fn branch_remote_name(&self, branch: String) -> Option<String> {
        self.config_string(format!("branch.{branch}.remote"))
    }

    /// The merge refspec configured for `branch` (e.g. `branch.main.merge`), if any.
    #[napi]
    pub fn branch_remote_ref(&self, branch: String) -> Option<String> {
        self.config_string(format!("branch.{branch}.merge"))
    }

    /// All key-value entries in the merged git configuration.
    #[napi]
    pub fn config_entries(&self) -> Result<Vec<ConfigEntry>> {
        let r = thread_local(&self.inner);
        let snapshot = r.config_snapshot();
        let mut out = Vec::new();
        for section in snapshot.sections() {
            let section_name = section.header().name().to_string();
            let sub_name = section.header().subsection_name().map(|s| lossy(s));
            for key_name in section.value_names() {
                let full_key = match &sub_name {
                    Some(sub) => format!("{section_name}.{sub}.{key_name}"),
                    None => format!("{section_name}.{key_name}"),
                };
                for val in section.values(key_name.as_str()) {
                    out.push(ConfigEntry {
                        key: full_key.clone(),
                        value: lossy(&*val),
                    });
                }
            }
        }
        Ok(out)
    }

    /// The working directory, if this is not a bare repository.
    #[napi]
    pub fn work_dir(&self) -> Option<String> {
        let r = thread_local(&self.inner);
        r.workdir().map(|p| p.to_string_lossy().into_owned())
    }

    /// The `.git` directory.
    #[napi]
    pub fn git_dir(&self) -> String {
        let r = thread_local(&self.inner);
        r.git_dir().to_string_lossy().into_owned()
    }

    /// The common git directory (may differ from `git_dir` for linked worktrees).
    #[napi]
    pub fn common_dir(&self) -> String {
        let r = thread_local(&self.inner);
        r.common_dir().to_string_lossy().into_owned()
    }

    /// Whether this repository is bare.
    #[napi]
    pub fn is_bare(&self) -> bool {
        let r = thread_local(&self.inner);
        r.is_bare()
    }

    // ---- worktree ---------------------------------------------------------

    /// List all linked worktrees for this repository.
    #[napi]
    pub fn worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        let r = thread_local(&self.inner);
        let proxies = r.worktrees().map_err(|e| err("worktrees", e))?;
        let mut out = Vec::new();
        for p in proxies {
            let git_dir = p.git_dir().to_string_lossy().into_owned();
            out.push(WorktreeInfo {
                name: p.id().ok().map(|id| lossy(id)),
                git_dir,
                work_dir: None,
                is_main: false,
                is_locked: p.is_locked(),
            });
        }
        Ok(out)
    }

    /// Information about the main worktree, if available.
    #[napi]
    pub fn main_worktree(&self) -> Result<Option<WorktreeInfo>> {
        let r = thread_local(&self.inner);
        if r.is_bare() {
            return Ok(None);
        }
        Ok(Some(WorktreeInfo {
            name: None,
            git_dir: r.git_dir().to_string_lossy().into_owned(),
            work_dir: r.workdir().map(|w| w.to_string_lossy().into_owned()),
            is_main: true,
            is_locked: false,
        }))
    }

    /// Whether this repository handle is a linked worktree.
    #[napi]
    pub fn is_linked_worktree(&self) -> bool {
        let r = thread_local(&self.inner);
        matches!(r.kind(), gix::repository::Kind::LinkedWorkTree { .. })
    }

    /// The repository kind as a string: `Common` (ordinary), `Submodule`, or `LinkedWorkTree`.
    #[napi]
    pub fn kind(&self) -> String {
        let r = thread_local(&self.inner);
        format!("{:?}", r.kind())
    }

    /// The object hash kind (`sha1` / `sha256`).
    #[napi]
    pub fn object_hash(&self) -> String {
        let r = thread_local(&self.inner);
        format!("{:?}", r.object_hash())
    }

    // ---- commits / revisions --------------------------------------------

    /// Create a commit on `reference` and return its id.
    #[napi]
    pub fn commit(
        &self,
        reference: String,
        message: String,
        tree: String,
        parents: Vec<String>,
    ) -> Result<String> {
        let r = thread_local(&self.inner);
        let parent_ids: Vec<gix::ObjectId> = parents
            .iter()
            .map(|p| parse_oid(p))
            .collect::<Result<Vec<_>>>()?;
        let id = r
            .commit(
                reference,
                message.as_str(),
                parse_oid(&tree)?,
                parent_ids,
            )
            .map_err(|e| err("commit", e))?;
        Ok(id.to_string())
    }

    /// Resolve a single revision specification (e.g. `HEAD~1`, `main`, `<sha>`) to an id.
    #[napi]
    pub fn rev_parse_single(&self, spec: String) -> Result<String> {
        let r = thread_local(&self.inner);
        let id = r
            .rev_parse_single(spec.as_str())
            .map_err(|e| err("rev_parse_single", e))?;
        Ok(id.to_string())
    }

    /// Resolve a revision specification to an id (alias of `revParseSingle`).
    #[napi]
    pub fn rev_parse(&self, spec: String) -> Result<String> {
        self.rev_parse_single(spec)
    }

    /// Walk all reachable commits from the given tip ids (newest first).
    #[napi]
    pub fn rev_walk(&self, tips: Vec<String>) -> Result<Vec<String>> {
        let r = thread_local(&self.inner);
        let tip_ids: Vec<gix::ObjectId> = tips
            .iter()
            .map(|t| parse_oid(t))
            .collect::<Result<Vec<_>>>()?;
        let platform = r.rev_walk(tip_ids);
        let ids = platform
            .all()
            .map_err(|e| err("rev_walk", e))?
            .map(|id| id.map(|i| i.id.to_string()))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| err("rev_walk item", e))?;
        Ok(ids)
    }

    /// The merge base of two commits, if any.
    #[napi]
    pub fn merge_base(&self, one: String, two: String) -> Result<Option<String>> {
        let r = thread_local(&self.inner);
        let base = r
            .merge_base(parse_oid(&one)?, parse_oid(&two)?)
            .map_err(|e| err("merge_base", e))?;
        Ok(base.map(|id| id.to_string()))
    }

    /// The (octopus) merge base of multiple commits, if any.
    #[napi]
    pub fn merge_base_octopus(&self, commits: Vec<String>) -> Result<Option<String>> {
        let r = thread_local(&self.inner);
        let ids: Vec<gix::ObjectId> = commits
            .iter()
            .map(|c| parse_oid(c))
            .collect::<Result<Vec<_>>>()?;
        let base = r
            .merge_base_octopus(ids)
            .map_err(|e| err("merge_base_octopus", e))?;
        Ok(base.map(|id| id.to_string()))
    }

    /// Whether `ancestor` commit is an ancestor of `descendant` commit.
    #[napi]
    pub fn is_ancestor(&self, ancestor: String, descendant: String) -> Result<bool> {
        let r = thread_local(&self.inner);
        let anc_id = parse_oid(&ancestor)?;
        let desc_id = parse_oid(&descendant)?;
        if anc_id == desc_id {
            return Ok(true);
        }
        let base = r.merge_base(anc_id, desc_id).map_err(|e| err("merge_base", e))?;
        Ok(base.map(|b| b.detach()) == Some(anc_id))
    }

    /// Describe a commit or HEAD (like `git describe`).
    #[napi]
    pub fn describe(&self, rev: Option<String>) -> Result<String> {
        let r = thread_local(&self.inner);
        let id: gix::ObjectId = match rev {
            Some(s) => match parse_oid(&s) {
                Ok(oid) => oid,
                Err(_) => r
                    .rev_parse_single(s.as_str())
                    .map_err(|e| err("rev_parse_single", e))?
                    .detach(),
            },
            None => r.head_id().map_err(|e| err("head_id", e))?.detach(),
        };
        let commit = r.find_commit(id).map_err(|e| err("find_commit", e))?;
        let desc = commit
            .describe()
            .format()
            .map_err(|e| err("describe", e))?;
        Ok(desc.to_string())
    }

    /// Whether this repository is shallow.
    #[napi]
    pub fn is_shallow(&self) -> bool {
        let r = thread_local(&self.inner);
        r.is_shallow().unwrap_or(false)
    }

    // ---- attributes / excludes / mailmap ----------------------------------

    /// Whether `path` is ignored according to `.gitignore` and exclude rules.
    #[napi]
    pub fn is_ignored(&self, path: String) -> Result<bool> {
        let r = thread_local(&self.inner);
        let index = r.index_or_empty().map_err(|e| err("index_or_empty", e))?;
        let mut stack = r
            .excludes(&index, None, Default::default())
            .map_err(|e| err("excludes", e))?;
        let platform = stack
            .at_path(path.as_str(), None)
            .map_err(|e| err("at_path", e))?;
        Ok(platform.is_excluded())
    }

    // ---- submodules / blame / state / identity ----------------------------

    /// List submodules defined in this repository.
    #[napi]
    pub fn submodules(&self) -> Result<Vec<SubmoduleInfo>> {
        let r = thread_local(&self.inner);
        let Some(submodules) = r.submodules().map_err(|e| err("submodules", e))? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for sm in submodules {
            let path = lossy(&sm.path().map_err(|e| err("path", e))?);
            let url = sm.url().ok().map(|u| u.to_bstring().to_string());
            out.push(SubmoduleInfo {
                name: lossy(sm.name()),
                path,
                url,
            });
        }
        Ok(out)
    }

    /// Default committer signature configured in this repository or environment.
    #[napi]
    pub fn committer(&self) -> Option<Signature> {
        let r = thread_local(&self.inner);
        let c = r.committer()?.ok()?;
        let (time, timezone_offset) = c
            .time()
            .ok()
            .map(|t| (t.seconds, t.offset))
            .unwrap_or((0, 0));
        Some(Signature {
            name: lossy(c.name),
            email: lossy(c.email),
            time,
            timezone_offset,
        })
    }

    /// Default author signature configured in this repository or environment.
    #[napi]
    pub fn author(&self) -> Option<Signature> {
        let r = thread_local(&self.inner);
        let a = r.author()?.ok()?;
        let (time, timezone_offset) = a
            .time()
            .ok()
            .map(|t| (t.seconds, t.offset))
            .unwrap_or((0, 0));
        Some(Signature {
            name: lossy(a.name),
            email: lossy(a.email),
            time,
            timezone_offset,
        })
    }

    /// Current repository operation state: `Clean`, `Merge`, `Rebase`, `CherryPick`, etc.
    #[napi]
    pub fn state(&self) -> String {
        let r = thread_local(&self.inner);
        format!("{:?}", r.state())
    }

    /// Resolve an author/committer signature through `.mailmap`, if available.
    #[napi]
    pub fn resolve_mailmap(&self, name: String, email: String) -> Signature {
        let r = thread_local(&self.inner);
        let mailmap = r.open_mailmap();
        let sig = gix::actor::SignatureRef {
            name: name.as_str().into(),
            email: email.as_str().into(),
            time: "",
        };
        let resolved = mailmap.resolve(sig);
        Signature {
            name: lossy(&resolved.name),
            email: lossy(&resolved.email),
            time: 0,
            timezone_offset: 0,
        }
    }

    // ---- merge / rebase --------------------------------------------------

    /// Merge two trees using an optional ancestor tree, returning the merged tree ID and any conflict paths.
    #[napi]
    pub fn merge_trees(
        &self,
        our_tree: String,
        their_tree: String,
        ancestor_tree: Option<String>,
    ) -> Result<MergeResult> {
        let r = thread_local(&self.inner);
        let our_id = parse_oid(&our_tree)?;
        let their_id = parse_oid(&their_tree)?;
        let anc_id = match ancestor_tree {
            Some(a) => parse_oid(&a)?,
            None => gix::hash::ObjectId::empty_tree(r.object_hash()),
        };
        let mut outcome = r
            .merge_trees(
                &anc_id,
                &our_id,
                &their_id,
                gix::merge::blob::builtin_driver::text::Labels::default(),
                gix::merge::tree::Options::default(),
            )
            .map_err(|e| err("merge_trees", e))?;
        let tree_id = outcome.tree.write().map_err(|e| err("write merged tree", e))?.to_string();
        let has_conflicts = !outcome.conflicts.is_empty();
        Ok(MergeResult {
            tree_id,
            conflicts: vec![],
            has_conflicts,
        })
    }

    // ---- log ---------------------------------------------------------------

    /// Read commit history (a "git log") starting at `rev` (or HEAD).
    ///
    /// Returns commit summaries in reverse-chronological order. `limit` caps the number of
    /// commits; `firstParent` restricts the walk to first-parent history. This resolves the
    /// whole range in one call (no per-commit round-trips).
    #[napi]
    pub fn log(
        &self,
        rev: Option<String>,
        limit: Option<f64>,
        first_parent: Option<bool>,
    ) -> Result<Vec<LogEntry>> {
        let r = thread_local(&self.inner);
        let tip: gix::ObjectId = match &rev {
            Some(s) => r
                .rev_parse_single(s.as_str())
                .map_err(|e| err("rev_parse_single", e))?
                .into(),
            None => r.head_id().map_err(|e| err("head_id", e))?.into(),
        };
        let limit = limit.map(|l| l as usize);
        let mut out = Vec::new();

        if first_parent.unwrap_or(false) {
            let mut current = Some(tip);
            while let Some(id) = current {
                let (entry, parents) = log_summary(&r, id)?;
                out.push(entry);
                if let Some(l) = limit {
                    if out.len() >= l {
                        break;
                    }
                }
                current = parents
                    .into_iter()
                    .next()
                    .map(|p| parse_oid(&p))
                    .transpose()?;
            }
        } else {
            let platform = r.rev_walk([tip]);
            for id in platform.all().map_err(|e| err("rev_walk", e))? {
                let id = id.map_err(|e| err("rev_walk item", e))?;
                let (entry, _parents) = log_summary(&r, id.id)?;
                out.push(entry);
                if let Some(l) = limit {
                    if out.len() >= l {
                        break;
                    }
                }
            }
        }
        Ok(out)
    }

    // ---- diff -------------------------------------------------------------

    /// Diff two trees (or one tree against empty when `old` is omitted).
    #[napi]
    pub fn diff_tree_to_tree(&self, old: Option<String>, new_id: String) -> Result<Vec<Change>> {
        let r = thread_local(&self.inner);
        let old_tree = match old {
            Some(o) => Some(r.find_tree(parse_oid(&o)?).map_err(|e| err("find_tree", e))?),
            None => None,
        };
        let new_tree = r
            .find_tree(parse_oid(&new_id)?)
            .map_err(|e| err("find_tree", e))?;
        let changes = r
            .diff_tree_to_tree(old_tree.as_ref(), Some(&new_tree), None)
            .map_err(|e| err("diff_tree_to_tree", e))?;
        Ok(changes
            .into_iter()
            .map(|c| match c {
                gix::object::tree::diff::ChangeDetached::Addition { location, id, .. } => Change {
                    path: lossy(&*location),
                    old_id: None,
                    new_id: Some(id.to_string()),
                    kind: "Added".to_string(),
                },
                gix::object::tree::diff::ChangeDetached::Deletion { location, id, .. } => Change {
                    path: lossy(&*location),
                    old_id: Some(id.to_string()),
                    new_id: None,
                    kind: "Deleted".to_string(),
                },
                gix::object::tree::diff::ChangeDetached::Modification {
                    location,
                    previous_id,
                    id,
                    ..
                } => Change {
                    path: lossy(&*location),
                    old_id: Some(previous_id.to_string()),
                    new_id: Some(id.to_string()),
                    kind: "Modified".to_string(),
                },
                gix::object::tree::diff::ChangeDetached::Rewrite {
                    location,
                    source_id,
                    id,
                    ..
                } => Change {
                    path: lossy(&*location),
                    old_id: Some(source_id.to_string()),
                    new_id: Some(id.to_string()),
                    kind: "Renamed".to_string(),
                },
            })
            .collect())
    }
}

/// A single change produced by [`Repository::diff_tree_to_tree`].
#[derive(Clone)]
#[napi(object)]
pub struct Change {
    pub path: String,
    pub old_id: Option<String>,
    pub new_id: Option<String>,
    /// The kind of change: `Added`, `Modified`, `Deleted`, `Renamed`, `Copied`.
    pub kind: String,
}

/// A single commit summary returned by [`Repository::log`].
#[derive(Clone)]
#[napi(object)]
pub struct LogEntry {
    pub id: String,
    /// Full commit message.
    pub message: String,
    /// First line (subject) of the message.
    pub subject: String,
    pub author_name: String,
    pub author_email: String,
    /// Authoring time, unix seconds.
    pub author_time: i64,
    pub committer_name: String,
    pub committer_email: String,
    /// Committing time, unix seconds.
    pub committer_time: i64,
    pub parent_ids: Vec<String>,
    pub tree_id: String,
}

/// Decode a commit into owned data for [`LogEntry`], returning the entry plus its parent ids.
fn log_summary(r: &gix::Repository, id: gix::ObjectId) -> Result<(LogEntry, Vec<String>)> {
    let commit = r.find_commit(id).map_err(|e| err("find_commit", e))?;
    let c = commit.decode().map_err(|e| err("decode commit", e))?;
    let author = c.author().map_err(|e| err("author", e))?;
    let committer = c.committer().map_err(|e| err("committer", e))?;
    let at = author.time().map_err(|e| err("author time", e))?;
    let ct = committer.time().map_err(|e| err("committer time", e))?;
    let full = lossy(c.message);
    let subject = full.split('\n').next().unwrap_or("").to_string();
    let parent_ids: Vec<String> = c.parents().map(|p| p.to_string()).collect();
    let entry = LogEntry {
        id: id.to_string(),
        message: full,
        subject,
        author_name: lossy(author.name),
        author_email: lossy(author.email),
        author_time: at.seconds,
        committer_name: lossy(committer.name),
        committer_email: lossy(committer.email),
        committer_time: ct.seconds,
        parent_ids: parent_ids.clone(),
        tree_id: c.tree().to_string(),
    };
    Ok((entry, parent_ids))
}
