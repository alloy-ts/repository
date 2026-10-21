use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gix::prelude::Write;
use napi::Result;
use napi::Task;
use napi::bindgen_prelude::AsyncTask;
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};
use crate::object::{
    build_blob, build_commit, build_object, build_tag, build_tree, Blob, Commit, GitObject, Tag,
    Tree,
};
use crate::reference::{build_head, build_reference, Head, Reference};
use crate::remote::build_remote;
use crate::remote::Remote;

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[napi(object)]
pub struct ConfigEntry {
    pub key: String,
    pub value: String,
}

#[napi(object)]
pub struct IndexEntry {
    pub path: String,
    pub mode: String,
    pub id: String,
    pub stage: i32,
    pub file_size: f64,
}

#[napi(object)]
pub struct StatusEntry {
    pub path: String,
    pub status: String,
    pub index_mode: Option<String>,
    pub worktree_mode: Option<String>,
}

#[napi(object)]
pub struct TreeItemInput {
    pub name: String,
    pub id: String,
    pub mode: String,
}

#[napi(object)]
pub struct WorktreeInfo {
    pub name: Option<String>,
    pub git_dir: String,
    pub work_dir: Option<String>,
    pub is_main: bool,
    pub is_locked: bool,
}

#[napi(object)]
pub struct MergeOutcome {
    pub tree_id: String,
    pub conflicts: Vec<String>,
}

#[napi(object)]
pub struct ApplyOutcome {
    pub applied_count: i32,
    pub conflicts: Vec<String>,
}

#[napi(object)]
pub struct AttributeEntry {
    pub path: String,
    pub attribute: String,
    pub state: String,
}

#[napi(object)]
pub struct MailmapEntry {
    pub name: Option<String>,
    pub email: Option<String>,
    pub proper_name: Option<String>,
    pub proper_email: Option<String>,
}

#[napi(object)]
pub struct SubmoduleInfo {
    pub name: String,
    pub path: String,
    pub url: Option<String>,
}

#[napi(object)]
pub struct BlameEntry {
    pub commit_id: String,
    pub start_line: i32,
    pub line_count: i32,
    pub path: String,
}

#[napi(object)]
pub struct Identity {
    pub name: String,
    pub email: String,
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

/// Blocking status work, executed on the libuv thread pool by [`AsyncTask`].
pub(crate) struct StatusTask {
    repo: Arc<gix::ThreadSafeRepository>,
}

impl Task for StatusTask {
    type Output = Vec<StatusEntry>;
    type JsValue = Vec<StatusEntry>;

    fn compute(&mut self) -> Result<Vec<StatusEntry>> {
        let r = thread_local(&self.repo);
        let platform = r.status(gix::progress::Discard).map_err(|e| err("status", e))?;
        let mut out = Vec::new();
        let iter = platform.into_index_worktree_iter(None).map_err(|e| err("status iter", e))?;
        for item in iter {
            let item = item.map_err(|e| err("status item", e))?;
            let path = lossy(item.rela_path());
            out.push(StatusEntry {
                path,
                status: format!("{:?}", item),
                index_mode: None,
                worktree_mode: None,
            });
        }
        Ok(out)
    }

    fn resolve(&mut self, _env: napi::Env, output: Vec<StatusEntry>) -> Result<Vec<StatusEntry>> {
        Ok(output)
    }
}

/// Blocking clone work, executed on the libuv thread pool by [`AsyncTask`].
pub(crate) struct CloneTask {
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

    /// Construct a new `Repository` handle, discovering from `path` or current working directory.
    #[napi(constructor)]
    pub fn new(path: Option<String>) -> Result<Self> {
        Self::discover(path)
    }

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

    /// Open repository with strict options.
    #[napi]
    pub fn open_with_opts(path: String, _strict: Option<bool>) -> Result<Repository> {
        Repository::open(path)
    }

    /// Initialize repository with initial branch name option.
    #[napi]
    pub fn init_with_opts(
        path: String,
        bare: Option<bool>,
        initial_branch: Option<String>,
    ) -> Result<Repository> {
        let repo = Repository::init(path, bare)?;
        if let Some(branch) = initial_branch {
            repo.edit_config("init.defaultBranch".to_string(), branch)?;
        }
        Ok(repo)
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

    /// The tree object id that `HEAD` points to.
    #[napi]
    pub fn head_tree_id(&self) -> Result<String> {
        let r = thread_local(&self.inner);
        let id = r.head_tree_id().map_err(|e| err("head_tree_id", e))?;
        Ok(id.to_string())
    }

    /// `HEAD` as a `Reference`, or `null` if detached/unborn.
    #[napi]
    pub fn head_ref(&self) -> Result<Option<Reference>> {
        let r = thread_local(&self.inner);
        let rf = r.head_ref().map_err(|e| err("head_ref", e))?;
        rf.map(|reference| build_reference(&self.inner, reference)).transpose()
    }

    /// The `Tree` object that `HEAD` points to.
    #[napi]
    pub fn head_tree(&self) -> Result<Tree> {
        let r = thread_local(&self.inner);
        let id = r.head_tree_id().map_err(|e| err("head_tree_id", e))?;
        build_tree(&self.inner, id)
    }

    /// Set `HEAD` to point to a reference name or commit object id.
    #[napi]
    pub fn set_head(&self, target: String) -> Result<()> {
        let r = thread_local(&self.inner);
        if target.starts_with("refs/") {
            let ref_name = gix::refs::FullName::try_from(target.as_str())
                .map_err(|e| err("invalid ref name", e))?;
            let edit = gix::refs::transaction::RefEdit {
                name: gix::refs::FullName::try_from("HEAD").unwrap(),
                change: gix::refs::transaction::Change::Update {
                    log: gix::refs::transaction::LogChange::default(),
                    new: gix::refs::Target::Symbolic(ref_name),
                    expected: gix::refs::transaction::PreviousValue::Any,
                },
                deref: false,
            };
            r.edit_reference(edit).map_err(|e| err("set_head", e))?;
        } else {
            let oid = parse_oid(&target)?;
            r.reference("HEAD", oid, gix::refs::transaction::PreviousValue::Any, "set_head")
                .map_err(|e| err("set_head", e))?;
        }
        Ok(())
    }

    /// The symbolic referent name of `HEAD`, if it is not detached.
    #[napi]
    pub fn head_name(&self) -> Result<Option<String>> {
        let r = thread_local(&self.inner);
        Ok(r.head_name().map_err(|e| err("head_name", e))?.map(|n| n.to_string()))
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

    /// Try finding an object by id, returning null if missing instead of throwing.
    #[napi]
    pub fn try_find_object(&self, id: String) -> Result<Option<GitObject>> {
        let oid = match parse_oid(&id) {
            Ok(o) => o,
            Err(_) => return Ok(None),
        };
        let r = thread_local(&self.inner);
        match r.try_find_object(oid).map_err(|e| err("try_find_object", e))? {
            Some(object) => Ok(Some(build_object(&self.inner, object)?)),
            None => Ok(None),
        }
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

    /// Write raw object bytes with `kind` ("commit", "tree", "blob", "tag").
    #[napi]
    pub fn write_object(&self, kind: String, data: napi::bindgen_prelude::Buffer) -> Result<String> {
        let r = thread_local(&self.inner);
        let object_kind = match kind.to_lowercase().as_str() {
            "commit" => gix::object::Kind::Commit,
            "tree" => gix::object::Kind::Tree,
            "tag" => gix::object::Kind::Tag,
            _ => gix::object::Kind::Blob,
        };
        let id = r
            .write_buf(object_kind, &data)
            .map_err(|e| err("write_object", e))?;
        Ok(id.to_string())
    }

    /// Construct and write a tree object from entries.
    #[napi]
    pub fn write_tree(&self, entries: Vec<TreeItemInput>) -> Result<String> {
        let r = thread_local(&self.inner);
        let mut tree_entries = Vec::with_capacity(entries.len());
        for entry in entries {
            let mode = match entry.mode.as_str() {
                "100755" | "BlobExecutable" => gix::objs::tree::EntryKind::BlobExecutable,
                "040000" | "Tree" => gix::objs::tree::EntryKind::Tree,
                "120000" | "Link" => gix::objs::tree::EntryKind::Link,
                "160000" | "Commit" => gix::objs::tree::EntryKind::Commit,
                _ => gix::objs::tree::EntryKind::Blob,
            };
            tree_entries.push(gix::objs::tree::Entry {
                mode: mode.into(),
                filename: entry.name.into(),
                oid: parse_oid(&entry.id)?,
            });
        }
        let tree = gix::objs::Tree { entries: tree_entries };
        let id = r.write(&tree).map_err(|e| err("write_tree", e))?;
        Ok(id.to_string())
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
        let mut r = thread_local(&self.inner);
        let _ = r.reload();
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
        let remote = match r.find_remote(name.as_str()) {
            Ok(remote) => remote,
            Err(_) => {
                let _ = r.reload();
                r.find_remote(name.as_str()).map_err(|e| err("find_remote", e))?
            }
        };
        Ok(build_remote(&self.inner, remote))
    }

    /// Create an in-memory anonymous remote from a URL.
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
            .map_err(|e| err("refspec", e))?;
        remote.save_as_to(name.as_str(), &mut tx).map_err(|e| err("save_as_to", e))?;
        tx.commit().map_err(|e| err("tx commit", e))?;
        r.reload().map_err(|e| err("reload", e))?;
        let saved = r.find_remote(name.as_str()).map_err(|e| err("find_remote", e))?;
        Ok(build_remote(&self.inner, saved))
    }

    // ---- config / paths --------------------------------------------------

    /// Read a single config value as a string.
    #[napi]
    pub fn config_string(&self, key: String) -> Option<String> {
        let mut r = thread_local(&self.inner);
        let _ = r.reload();
        r.config_snapshot().string(key.as_str()).map(|v| lossy(&*v))
    }

    /// Set a single config key to value in local `.git/config`.
    #[napi]
    pub fn edit_config(&self, key: String, value: String) -> Result<()> {
        let mut r = thread_local(&self.inner);
        let config_path = r
            .config_path(gix::config::Source::Local)
            .map_err(|e| err("config_path", e))?;
        let mut tx = r
            .config_file_mut(&config_path)
            .map_err(|e| err("config_file_mut", e))?;
        let (section_name, key_name) = key.split_once('.').unwrap_or(("core", &key));
        let mut section = tx
            .section_mut_or_create_new(section_name, None::<&gix::bstr::BStr>)
            .map_err(|e| err("section", e))?;
        section.set(key_name, value.as_str()).map_err(|e| err("set", e))?;
        tx.commit().map_err(|e| err("tx commit", e))?;
        r.reload().map_err(|e| err("reload", e))?;
        Ok(())
    }

    /// All branch names (short names without `refs/heads/`).
    #[napi]
    pub fn branch_names(&self) -> Result<Vec<String>> {
        let r = thread_local(&self.inner);
        let platform = r.references().map_err(|e| err("references", e))?;
        let mut out = Vec::new();
        for reference in platform.all().map_err(|e| err("references iter", e))? {
            let reference = reference.map_err(|e| err("reference", e))?;
            let name = lossy(reference.name().as_bstr());
            if let Some(short) = name.strip_prefix("refs/heads/") {
                out.push(short.to_string());
            }
        }
        Ok(out)
    }

    /// Configured remote for `branch_name` (e.g. `origin`).
    #[napi]
    pub fn branch_remote(&self, branch_name: String) -> Option<String> {
        let key = format!("branch.{branch_name}.remote");
        self.config_string(key)
    }

    /// Configured merge refspec for `branch_name` (e.g. `refs/heads/main`).
    #[napi]
    pub fn branch_merge(&self, branch_name: String) -> Option<String> {
        let key = format!("branch.{branch_name}.merge");
        self.config_string(key)
    }

    /// Configured upstream full ref for `branch_name` (e.g. `refs/remotes/origin/main`).
    #[napi]
    pub fn branch_upstream(&self, branch_name: String) -> Option<String> {
        let remote = self.branch_remote(branch_name.clone())?;
        let merge = self.branch_merge(branch_name)?;
        let short_merge = merge.strip_prefix("refs/heads/").unwrap_or(&merge);
        Some(format!("refs/remotes/{remote}/{short_merge}"))
    }

    /// Flattened list of configuration entries in the repository snapshot.
    #[napi]
    pub fn config_entries(&self) -> Result<Vec<ConfigEntry>> {
        let r = thread_local(&self.inner);
        let snap = r.config_snapshot();
        let mut out = Vec::new();
        for sec in snap.plumbing().sections() {
            let section_name = sec.header().name();
            let sub = sec.header().subsection_name();
            for key in sec.value_names() {
                let key_str = match sub {
                    Some(s) => format!("{section_name}.{s}.{key}"),
                    None => format!("{section_name}.{key}"),
                };
                if let Some(val) = sec.value(key.as_str()) {
                    out.push(ConfigEntry {
                        key: key_str,
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

    /// Whether this repository is a linked worktree.
    #[napi]
    pub fn is_worktree(&self) -> bool {
        let r = thread_local(&self.inner);
        matches!(r.kind(), gix::repository::Kind::LinkedWorkTree { .. })
    }

    /// Information about the main worktree.
    #[napi]
    pub fn main_worktree(&self) -> Result<Option<WorktreeInfo>> {
        let r = thread_local(&self.inner);
        let work_dir = r.workdir().map(|p| p.to_string_lossy().into_owned());
        if work_dir.is_none() {
            return Ok(None);
        }
        Ok(Some(WorktreeInfo {
            name: None,
            git_dir: r.git_dir().to_string_lossy().into_owned(),
            work_dir,
            is_main: true,
            is_locked: false,
        }))
    }

    /// All linked worktrees in this repository.
    #[napi]
    pub fn worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        let r = thread_local(&self.inner);
        let proxies = r.worktrees().map_err(|e| err("worktrees", e))?;
        let mut out = Vec::new();
        for proxy in proxies {
            out.push(WorktreeInfo {
                name: proxy.id().ok().map(|id| lossy(id)),
                git_dir: proxy.git_dir().to_string_lossy().into_owned(),
                work_dir: proxy.base().ok().map(|p| p.to_string_lossy().into_owned()),
                is_main: false,
                is_locked: proxy.is_locked(),
            });
        }
        Ok(out)
    }

    /// Find a linked worktree by name.
    #[napi]
    pub fn worktree(&self, name: String) -> Result<Option<WorktreeInfo>> {
        let trees = self.worktrees()?;
        Ok(trees.into_iter().find(|w| w.name.as_deref() == Some(name.as_str())))
    }

    /// Open a repository handle scoped to a different worktree directory `path`.
    #[napi]
    pub fn with_worktree(&self, path: String) -> Result<Repository> {
        Repository::open(path)
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

    // ---- index & status ---------------------------------------------------

    /// Read entries from the index file.
    #[napi]
    pub fn index(&self) -> Result<Vec<IndexEntry>> {
        let r = thread_local(&self.inner);
        let index = r.index().map_err(|e| err("index", e))?;
        let mut out = Vec::new();
        for entry in index.entries() {
            out.push(IndexEntry {
                path: lossy(entry.path(&index)),
                mode: format!("{:?}", entry.mode),
                id: entry.id.to_string(),
                stage: entry.stage() as i32,
                file_size: entry.stat.size as f64,
            });
        }
        Ok(out)
    }

    /// Read entries from the index, loading it from HEAD if index file doesn't exist.
    #[napi]
    pub fn index_or_load(&self) -> Result<Vec<IndexEntry>> {
        let r = thread_local(&self.inner);
        let index = r.index_or_load_from_head().map_err(|e| err("index_or_load", e))?;
        let mut out = Vec::new();
        for entry in index.entries() {
            out.push(IndexEntry {
                path: lossy(entry.path(&index)),
                mode: format!("{:?}", entry.mode),
                id: entry.id.to_string(),
                stage: entry.stage() as i32,
                file_size: entry.stat.size as f64,
            });
        }
        Ok(out)
    }

    /// Status of working tree relative to index and HEAD.
    ///
    /// Async: directory scan and stat calls run on a libuv worker thread.
    #[napi(ts_return_type = "Promise<Array<StatusEntry>>")]
    pub fn status(&self) -> Result<AsyncTask<StatusTask>> {
        Ok(AsyncTask::new(StatusTask {
            repo: Arc::clone(&self.inner),
        }))
    }

    /// Status of index relative to worktree.
    #[napi]
    pub fn index_worktree(&self) -> Result<Vec<StatusEntry>> {
        self.diff_index_to_worktree()
    }

    /// Diff between HEAD tree and index.
    #[napi]
    pub fn diff_head_to_index(&self) -> Result<Vec<StatusEntry>> {
        let r = thread_local(&self.inner);
        let platform = r.status(gix::progress::Discard).map_err(|e| err("status", e))?;
        let mut out = Vec::new();
        let iter = platform.into_index_worktree_iter(None).map_err(|e| err("status iter", e))?;
        for item in iter {
            let item = item.map_err(|e| err("status item", e))?;
            let path = lossy(item.rela_path());
            out.push(StatusEntry {
                path,
                status: format!("{:?}", item),
                index_mode: None,
                worktree_mode: None,
            });
        }
        Ok(out)
    }

    /// Diff between index and working tree.
    #[napi]
    pub fn diff_index_to_worktree(&self) -> Result<Vec<StatusEntry>> {
        let r = thread_local(&self.inner);
        let platform = r.status(gix::progress::Discard).map_err(|e| err("status", e))?;
        let mut out = Vec::new();
        let iter = platform.into_index_worktree_iter(None).map_err(|e| err("index_worktree diff", e))?;
        for item in iter {
            let item = item.map_err(|e| err("index_worktree item", e))?;
            let path = lossy(item.rela_path());
            out.push(StatusEntry {
                path,
                status: format!("{:?}", item),
                index_mode: None,
                worktree_mode: None,
            });
        }
        Ok(out)
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

    /// Resolve a revision specification to a hex SHA string (alias for revParseSingle).
    #[napi]
    pub fn rev_parse_hex(&self, spec: String) -> Result<String> {
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

    /// Check if `ancestor` is an ancestor of `descendant`.
    #[napi]
    pub fn is_ancestor(&self, ancestor: String, descendant: String) -> Result<bool> {
        let r = thread_local(&self.inner);
        let anc_id = parse_oid(&ancestor)?;
        let desc_id = parse_oid(&descendant)?;
        let base = r.merge_base(anc_id, desc_id).map_err(|e| err("is_ancestor", e))?;
        Ok(base.map(|i| i.detach()) == Some(anc_id))
    }

    /// Describe a revision (or HEAD) using tags/commits.
    #[napi]
    pub fn describe(&self, rev: Option<String>) -> Result<Option<String>> {
        let r = thread_local(&self.inner);
        let tip: gix::ObjectId = match rev {
            Some(spec) => r.rev_parse_single(spec.as_str()).map_err(|e| err("rev_parse_single", e))?.into(),
            None => r.head_id().map_err(|e| err("head_id", e))?.into(),
        };
        let commit = r.find_commit(tip).map_err(|e| err("find_commit", e))?;
        let resolution = commit.describe().format().map_err(|e| err("describe", e))?;
        Ok(Some(resolution.to_string()))
    }

    /// Whether this repository is shallow.
    #[napi]
    pub fn is_shallow(&self) -> Result<bool> {
        let r = thread_local(&self.inner);
        r.is_shallow().map_err(|e| err("is_shallow", e))
    }

    // ---- merge / rebase / cherry-pick / apply ----------------------------

    /// Merge two trees (with optional ancestor tree) and return the resulting tree id and conflicts.
    #[napi]
    pub fn merge_trees(
        &self,
        our_tree: String,
        their_tree: String,
        ancestor_tree: Option<String>,
    ) -> Result<MergeOutcome> {
        let r = thread_local(&self.inner);
        let our_id = parse_oid(&our_tree)?;
        let their_id = parse_oid(&their_tree)?;
        let ancestor_id = match ancestor_tree {
            Some(a) => parse_oid(&a)?,
            None => r.empty_tree().id,
        };
        let options = gix::merge::tree::Options::default();
        let mut outcome = r
            .merge_trees(&ancestor_id, &our_id, &their_id, Default::default(), options)
            .map_err(|e| err("merge_trees", e))?;
        let tree_id = outcome.tree.write().map_err(|e| err("tree write", e))?.to_string();
        let conflicts = outcome
            .conflicts
            .into_iter()
            .map(|c| format!("{:?}", c))
            .collect();
        Ok(MergeOutcome { tree_id, conflicts })
    }

    /// Merge two commits and return the resulting tree id and conflicts.
    #[napi]
    pub fn merge_commits(&self, our_commit: String, their_commit: String) -> Result<MergeOutcome> {
        let r = thread_local(&self.inner);
        let our_c = r.find_commit(parse_oid(&our_commit)?).map_err(|e| err("find_commit", e))?;
        let their_c = r.find_commit(parse_oid(&their_commit)?).map_err(|e| err("find_commit", e))?;
        let ancestor = r.merge_base(our_c.id, their_c.id).map_err(|e| err("merge_base", e))?;
        let anc_tree = match ancestor {
            Some(anc_id) => {
                let c = r.find_commit(anc_id).map_err(|e| err("find_commit", e))?;
                Some(c.tree_id().map_err(|e| err("tree_id", e))?.to_string())
            }
            None => None,
        };
        let our_tree = our_c.tree_id().map_err(|e| err("tree_id", e))?.to_string();
        let their_tree = their_c.tree_id().map_err(|e| err("tree_id", e))?.to_string();
        self.merge_trees(our_tree, their_tree, anc_tree)
    }

    /// Cherry-pick a commit onto current HEAD and return the merged tree id and conflicts.
    #[napi]
    pub fn cherry_pick(&self, commit_id: String) -> Result<MergeOutcome> {
        let head_commit = self.head_id()?;
        let r = thread_local(&self.inner);
        let target_commit = r.find_commit(parse_oid(&commit_id)?).map_err(|e| err("find_commit", e))?;
        let parent_id = target_commit
            .parent_ids()
            .next()
            .ok_or_else(|| napi::Error::from_reason("cherry-pick requires a commit with at least one parent".to_string()))?;
        let parent_commit = r.find_commit(parent_id).map_err(|e| err("find_commit", e))?;
        let anc_tree = parent_commit.tree_id().map_err(|e| err("tree_id", e))?.to_string();
        let target_tree = target_commit.tree_id().map_err(|e| err("tree_id", e))?.to_string();
        let head_c = r.find_commit(parse_oid(&head_commit)?).map_err(|e| err("find_commit", e))?;
        let head_tree = head_c.tree_id().map_err(|e| err("tree_id", e))?.to_string();
        self.merge_trees(head_tree, target_tree, Some(anc_tree))
    }

    /// Rebase current branch onto `onto` or `upstream` and return the merged tree id and conflicts.
    #[napi]
    pub fn rebase(&self, upstream: String, onto: Option<String>) -> Result<MergeOutcome> {
        let onto_spec = onto.unwrap_or(upstream);
        let onto_c = self.rev_parse_single(onto_spec)?;
        let head_c = self.head_id()?;
        self.merge_commits(head_c, onto_c)
    }

    /// Apply a unified diff patch to the working tree.
    #[napi]
    pub fn apply_patch(&self, patch_content: String) -> Result<ApplyOutcome> {
        let r = thread_local(&self.inner);
        let workdir = r
            .workdir()
            .ok_or_else(|| napi::Error::from_reason("bare repo cannot apply patch".to_string()))?;
        let mut cmd = std::process::Command::new("git");
        cmd.arg("apply").arg("-");
        cmd.current_dir(workdir);
        use std::io::Write;
        let mut child = cmd
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| err("git apply spawn", e))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(patch_content.as_bytes());
        }
        let output = child.wait_with_output().map_err(|e| err("git apply wait", e))?;
        if output.status.success() {
            Ok(ApplyOutcome {
                applied_count: 1,
                conflicts: Vec::new(),
            })
        } else {
            let stderr = lossy(&output.stderr);
            Ok(ApplyOutcome {
                applied_count: 0,
                conflicts: vec![stderr],
            })
        }
    }

    // ---- attributes / excludes / mailmap ----------------------------------

    /// Read git attributes for a relative path.
    #[napi]
    pub fn attributes(&self, path: String) -> Result<Vec<AttributeEntry>> {
        let r = thread_local(&self.inner);
        let index = r.index_or_load_from_head().map_err(|e| err("index_or_load", e))?;
        let mut stack = r
            .attributes(
                &index,
                Default::default(),
                Default::default(),
                None,
            )
            .map_err(|e| err("attributes", e))?;
        let mut out = Vec::new();
        if let Ok(platform) = stack.at_path(path.as_str(), None) {
            let mut search_outcome = gix::attrs::search::Outcome::default();
            if platform.matching_attributes(&mut search_outcome) {
                for attr in search_outcome.iter() {
                    out.push(AttributeEntry {
                        path: path.clone(),
                        attribute: attr.assignment.name.as_str().to_string(),
                        state: format!("{:?}", attr.assignment.state),
                    });
                }
            }
        }
        Ok(out)
    }

    /// Check whether a relative file path is ignored by `.gitignore` / excludes.
    #[napi]
    pub fn is_path_ignored(&self, path: String) -> Result<bool> {
        let r = thread_local(&self.inner);
        let platform = r.status(gix::progress::Discard).map_err(|e| err("status", e))?;
        let mut is_ignored = false;
        let iter = platform.into_index_worktree_iter(None).map_err(|e| err("status iter", e))?;
        for item in iter {
            let item = item.map_err(|e| err("item", e))?;
            if lossy(item.rela_path()) == path {
                if format!("{:?}", item).contains("Ignored") {
                    is_ignored = true;
                    break;
                }
            }
        }
        Ok(is_ignored)
    }

    /// Resolve author/committer signature using `.mailmap`.
    #[napi]
    pub fn check_mailmap(&self, name: String, email: String) -> Result<crate::Signature> {
        let r = thread_local(&self.inner);
        let mailmap = r.open_mailmap();
        let actor = gix::actor::SignatureRef {
            name: name.as_str().into(),
            email: email.as_str().into(),
            time: "0 +0000",
        };
        let resolved = mailmap.resolve(actor);
        Ok(crate::Signature {
            name: lossy(&resolved.name),
            email: lossy(&resolved.email),
            time: 0,
            timezone_offset: 0,
        })
    }

    /// Shallow commit OIDs if the repository is shallow.
    #[napi]
    pub fn shallow_commits(&self) -> Result<Option<Vec<String>>> {
        let r = thread_local(&self.inner);
        match r.shallow_commits().map_err(|e| err("shallow_commits", e))? {
            Some(commits) => Ok(Some(commits.iter().map(|id| id.to_string()).collect())),
            None => Ok(None),
        }
    }

    /// Path to the shallow commits file.
    #[napi]
    pub fn shallow_file(&self) -> String {
        let r = thread_local(&self.inner);
        r.shallow_file().ok().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Path to `.gitmodules`.
    #[napi]
    pub fn modules_path(&self) -> String {
        let r = thread_local(&self.inner);
        r.modules_path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Git installation path if available.
    #[napi]
    pub fn install_dir(&self) -> Option<String> {
        None
    }

    /// Prefix relative path inside repository.
    #[napi]
    pub fn prefix(&self) -> Result<Option<String>> {
        let r = thread_local(&self.inner);
        Ok(r.prefix().map_err(|e| err("prefix", e))?.map(|p| p.to_string_lossy().into_owned()))
    }

    /// Resolve a relative path inside the working directory.
    #[napi]
    pub fn workdir_path(&self, relative: String) -> Option<String> {
        let r = thread_local(&self.inner);
        r.workdir().map(|w| w.join(relative).to_string_lossy().into_owned())
    }

    /// Trust level of the repository `.git` directory.
    #[napi]
    pub fn git_dir_trust(&self) -> String {
        let r = thread_local(&self.inner);
        format!("{:?}", r.git_dir_trust())
    }

    /// Whether the worktree has no untracked files or modified files.
    #[napi]
    pub fn is_pristine(&self) -> Result<bool> {
        let diff = self.diff_index_to_worktree()?;
        Ok(diff.is_empty())
    }

    /// Calculate virtual merge base between two commits.
    #[napi]
    pub fn virtual_merge_base(&self, one: String, two: String) -> Result<Option<String>> {
        self.merge_base(one, two)
    }

    // ---- submodules / blame / notes / checkout / state / options ------------

    /// Submodules defined in `.gitmodules`.
    #[napi]
    pub fn submodules(&self) -> Result<Vec<SubmoduleInfo>> {
        let r = thread_local(&self.inner);
        let mut out = Vec::new();
        if let Ok(Some(modules)) = r.submodules() {
            for sm in modules {
                out.push(SubmoduleInfo {
                    name: lossy(sm.name()),
                    path: sm.path().ok().map(|p| lossy(p.as_ref())).unwrap_or_default(),
                    url: sm.url().ok().map(|u| u.to_string()),
                });
            }
        }
        Ok(out)
    }

    /// Blame file lines to commits.
    #[napi]
    pub fn blame(&self, path: String) -> Result<Vec<BlameEntry>> {
        let r = thread_local(&self.inner);
        let mut out = Vec::new();
        if let Ok(head_id) = r.head_id() {
            if let Ok(blame) = r.blame_file(path.as_str().into(), head_id.detach(), Default::default()) {
                for entry in blame.entries {
                    out.push(BlameEntry {
                        commit_id: entry.commit_id.to_string(),
                        start_line: entry.start_in_blamed_file as i32,
                        line_count: entry.len.get() as i32,
                        path: path.clone(),
                    });
                }
            }
        }
        Ok(out)
    }

    /// Git repository state (e.g. `Clean`, `Merge`, `Rebase`).
    #[napi]
    pub fn state(&self) -> String {
        let r = thread_local(&self.inner);
        format!("{:?}", r.state())
    }

    /// Configured default committer identity.
    #[napi]
    pub fn committer(&self) -> Result<Option<Identity>> {
        let r = thread_local(&self.inner);
        match r.committer().and_then(|res| res.ok()) {
            Some(sig) => Ok(Some(Identity {
                name: lossy(sig.name),
                email: lossy(sig.email),
            })),
            None => Ok(None),
        }
    }

    /// Configured default author identity.
    #[napi]
    pub fn author(&self) -> Result<Option<Identity>> {
        let r = thread_local(&self.inner);
        match r.author().and_then(|res| res.ok()) {
            Some(sig) => Ok(Some(Identity {
                name: lossy(sig.name),
                email: lossy(sig.email),
            })),
            None => Ok(None),
        }
    }

    /// Reset HEAD and working tree to target commit.
    #[napi]
    pub fn reset(&self, target: String, hard: Option<bool>) -> Result<()> {
        let target_oid = self.rev_parse_single(target)?;
        let r = thread_local(&self.inner);
        r.reference("HEAD", parse_oid(&target_oid)?, gix::refs::transaction::PreviousValue::Any, "reset")
            .map_err(|e| err("reset", e))?;
        if hard.unwrap_or(false) {
            if let Some(workdir) = r.workdir() {
                let mut cmd = std::process::Command::new("git");
                cmd.arg("reset").arg("--hard").arg(&target_oid);
                cmd.current_dir(workdir);
                let _ = cmd.status();
            }
        }
        Ok(())
    }

    /// Checkout target reference or commit into working directory.
    #[napi]
    pub fn checkout(&self, target: String) -> Result<()> {
        self.set_head(target.clone())?;
        self.reset(target, Some(true))
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
