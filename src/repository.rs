use std::sync::Arc;
use std::sync::atomic::AtomicBool;

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

fn parse_signature(s: &SignatureInput) -> gix::actor::Signature {
    let t = s.time.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    });
    let tz = s.timezone_offset.unwrap_or(0);
    gix::actor::Signature {
        name: s.name.as_str().into(),
        email: s.email.as_str().into(),
        time: gix::date::Time::new(t, tz),
    }
}

fn parse_tree_mode(mode: &str) -> Result<gix::object::tree::EntryMode> {
    let mode_bytes = match mode {
        "100644" | "Blob" => b"100644".as_slice(),
        "100755" | "BlobExecutable" => b"100755".as_slice(),
        "040000" | "Tree" => b"40000".as_slice(),
        "120000" | "Link" => b"120000".as_slice(),
        "160000" | "Commit" => b"160000".as_slice(),
        _ => return Err(err("parse_tree_mode", format!("invalid tree mode '{mode}'"))),
    };
    gix::object::tree::EntryMode::from_bytes(mode_bytes)
        .ok_or_else(|| err("parse_tree_mode", format!("failed to parse tree mode '{mode}'")))
}

fn parse_object_kind(kind: &str) -> Result<gix::object::Kind> {
    match kind.to_lowercase().as_str() {
        "commit" => Ok(gix::object::Kind::Commit),
        "tree" => Ok(gix::object::Kind::Tree),
        "blob" => Ok(gix::object::Kind::Blob),
        "tag" => Ok(gix::object::Kind::Tag),
        _ => Err(err("parse_object_kind", format!("invalid object kind '{kind}'"))),
    }
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
    pub size: u32,
}

#[napi(object)]
pub struct StatusEntry {
    pub path: String,
    pub status: String,
    pub index_status: Option<String>,
    pub worktree_status: Option<String>,
}

#[napi(object)]
pub struct SignatureInput {
    pub name: String,
    pub email: String,
    pub time: Option<i64>,
    pub timezone_offset: Option<i32>,
}

#[napi(object)]
pub struct TreeItemInput {
    pub name: String,
    pub mode: String,
    pub id: String,
}

#[napi(object)]
pub struct MergeOutcome {
    pub tree_id: Option<String>,
    pub conflicts: Vec<String>,
    pub has_conflicts: bool,
}

#[napi(object)]
pub struct RebaseOutcome {
    pub head_id: String,
    pub applied_commits: Vec<String>,
    pub has_conflicts: bool,
}

#[napi(object)]
pub struct AttributeEntry {
    pub name: String,
    pub value: String,
}

#[napi(object)]
pub struct MailmapEntry {
    pub name: Option<String>,
    pub email: String,
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
pub struct BlameHunk {
    pub commit_id: String,
    pub start_line: u32,
    pub line_count: u32,
    pub author_name: String,
    pub author_email: String,
}

#[napi(object)]
pub struct NoteEntry {
    pub annotated_id: String,
    pub note_id: String,
}

#[napi(object)]
pub struct OpenOptionsInput {
    pub strict: Option<bool>,
    pub isolate: Option<bool>,
}

#[napi(object)]
pub struct InitOptionsInput {
    pub bare: Option<bool>,
    pub object_hash: Option<String>,
}

#[napi(object)]
pub struct WorktreeInfo {
    pub name: Option<String>,
    pub path: String,
    pub git_dir: String,
    pub is_main: bool,
    pub is_locked: bool,
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

pub(crate) struct StatusTask {
    repo: Arc<gix::ThreadSafeRepository>,
}

impl Task for StatusTask {
    type Output = Vec<StatusEntry>;
    type JsValue = Vec<StatusEntry>;

    fn compute(&mut self) -> Result<Vec<StatusEntry>> {
        let r = self.repo.to_thread_local();
        let status = r.status(gix::progress::Discard).map_err(|e| err("status", e))?;
        let iter = status.into_index_worktree_iter(None).map_err(|e| err("status iter", e))?;
        let mut out = Vec::new();
        for entry in iter {
            let item = entry.map_err(|e| err("status item", e))?;
            let path = lossy(item.rela_path().as_ref());
            let status_str = format!("{:?}", item);
            out.push(StatusEntry {
                path,
                status: status_str.clone(),
                index_status: None,
                worktree_status: Some(status_str),
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

    /// Open repository with options.
    #[napi]
    pub fn open_opts(path: String, options: Option<OpenOptionsInput>) -> Result<Repository> {
        let mut opts = gix::open::Options::default();
        if let Some(o) = options {
            if o.strict.unwrap_or(false) {
                opts = opts.strict_config(true);
            }
        }
        let ts = gix::ThreadSafeRepository::open_opts(path, opts).map_err(|e| err("open_opts", e))?;
        Ok(Repository {
            inner: Arc::new(ts),
        })
    }

    /// Initialize repository with options.
    #[napi]
    pub fn init_opts(path: String, options: Option<InitOptionsInput>) -> Result<Repository> {
        let bare = options.as_ref().and_then(|o| o.bare).unwrap_or(false);
        let repo = if bare {
            gix::init_bare(path)
        } else {
            gix::init(path)
        }.map_err(|e| err("init_opts", e))?;
        Ok(Repository {
            inner: Arc::new(repo.into()),
        })
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

    /// The tree id `HEAD` points to.
    #[napi]
    pub fn head_tree_id(&self) -> Result<String> {
        let r = thread_local(&self.inner);
        let id = r.head_tree_id().map_err(|e| err("head_tree_id", e))?;
        Ok(id.to_string())
    }

    /// The tree `HEAD` points to.
    #[napi]
    pub fn head_tree(&self) -> Result<Tree> {
        let r = thread_local(&self.inner);
        let id = r.head_tree_id().map_err(|e| err("head_tree_id", e))?;
        build_tree(&self.inner, id)
    }

    /// The `HEAD` reference, or null if detached or unborn.
    #[napi]
    pub fn head_ref(&self) -> Result<Option<Reference>> {
        let r = thread_local(&self.inner);
        let rf = r.head_ref().map_err(|e| err("head_ref", e))?;
        rf.map(|reference| build_reference(&self.inner, reference)).transpose()
    }

    /// Set `HEAD` to point to a reference (e.g. `refs/heads/main`) or commit ID.
    #[napi]
    pub fn set_head(&self, name_or_id: String) -> Result<()> {
        let r = thread_local(&self.inner);
        let target = if name_or_id.starts_with("refs/") {
            let full_name = gix::refs::FullName::try_from(name_or_id.as_str())
                .map_err(|e| err("invalid ref name", e))?;
            gix::refs::Target::Symbolic(full_name)
        } else {
            let oid = parse_oid(&name_or_id)?;
            gix::refs::Target::Object(oid)
        };
        let edit = gix::refs::transaction::RefEdit {
            change: gix::refs::transaction::Change::Update {
                log: gix::refs::transaction::LogChange::default(),
                expected: gix::refs::transaction::PreviousValue::Any,
                new: target,
            },
            name: gix::refs::FullName::try_from("HEAD").unwrap(),
            deref: false,
        };
        r.edit_reference(edit).map_err(|e| err("set_head", e))?;
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
        let r = thread_local(&self.inner);
        let remote = r
            .find_remote(name.as_str())
            .map_err(|e| err("find_remote", e))?;
        Ok(build_remote(&self.inner, remote))
    }

    /// Create an anonymous remote for a URL.
    #[napi]
    pub fn remote_at(&self, url: String) -> Result<Remote> {
        let r = thread_local(&self.inner);
        let remote = r.remote_at(url.as_str()).map_err(|e| err("remote_at", e))?;
        Ok(build_remote(&self.inner, remote))
    }

    /// Create and save a named remote for a URL.
    #[napi]
    pub fn create_remote(&self, name: String, url: String) -> Result<Remote> {
        let r = thread_local(&self.inner);
        let mut remote = r.remote_at(url.as_str()).map_err(|e| err("create_remote", e))?;
        let config_path = r.git_dir().join("config");
        let mut config = if config_path.exists() {
            gix::config::File::from_path_no_includes(config_path.clone(), gix::config::Source::Local)
                .map_err(|e| err("read config", e))?
        } else {
            gix::config::File::default()
        };
        remote.save_as_to(name.as_str(), &mut config).map_err(|e| err("create_remote save_as_to", e))?;
        let mut file = std::fs::File::create(&config_path).map_err(|e| err("create config file", e))?;
        config.write_to(&mut file).map_err(|e| err("write config", e))?;
        Ok(build_remote(&self.inner, remote))
    }

    // ---- config / paths --------------------------------------------------

    /// Read a single config value as a string.
    #[napi]
    pub fn config_string(&self, key: String) -> Option<String> {
        let r = thread_local(&self.inner);
        r.config_snapshot().string(key.as_str()).map(|v| lossy(&*v))
    }

    /// Return all flattened config entries.
    #[napi]
    pub fn config_entries(&self) -> Result<Vec<ConfigEntry>> {
        let r = thread_local(&self.inner);
        let snap = r.config_snapshot();
        let mut out = Vec::new();
        for section in snap.sections() {
            let section_name = lossy(section.header().name().as_ref());
            let sub_name = section.header().subsection_name().map(|s| lossy(s.as_ref()));
            for key in section.value_names() {
                let key_str = key.as_str();
                let full_key = match &sub_name {
                    Some(sub) => format!("{section_name}.{sub}.{key_str}"),
                    None => format!("{section_name}.{key_str}"),
                };
                if let Some(v) = section.value(key_str) {
                    out.push(ConfigEntry {
                        key: full_key,
                        value: lossy(&*v),
                    });
                }
            }
        }
        Ok(out)
    }

    /// Read single config value or return all entries as key-value pairs.
    #[napi]
    pub fn config(&self, key: Option<String>) -> Result<Vec<ConfigEntry>> {
        let r = thread_local(&self.inner);
        match key {
            Some(k) => {
                let snap = r.config_snapshot();
                match snap.string(k.as_str()) {
                    Some(v) => Ok(vec![ConfigEntry {
                        key: k,
                        value: lossy(&*v),
                    }]),
                    None => Ok(Vec::new()),
                }
            }
            None => self.config_entries(),
        }
    }

    /// Alias for `config_entries()`.
    #[napi]
    pub fn config_snapshot(&self) -> Result<Vec<ConfigEntry>> {
        self.config_entries()
    }

    /// Set a configuration value in local `.git/config`.
    #[napi]
    pub fn edit_config(&self, key: String, value: String) -> Result<()> {
        let r = thread_local(&self.inner);
        let config_path = r.git_dir().join("config");
        let mut config = if config_path.exists() {
            gix::config::File::from_path_no_includes(config_path.clone(), gix::config::Source::Local)
                .map_err(|e| err("read config", e))?
        } else {
            gix::config::File::default()
        };

        let parts: Vec<&str> = key.split('.').collect();
        if parts.len() < 2 {
            return Err(err("edit_config", "key must have at least section and key name (e.g. 'user.name')"));
        }

        let (section, sub, key_name) = match parts.len() {
            2 => (parts[0], None, parts[1]),
            _ => (parts[0], Some(parts[1..parts.len() - 1].join(".")), parts[parts.len() - 1]),
        };

        config.set_raw_value_by(
            section,
            sub.as_deref().map(gix::bstr::BStr::new),
            key_name,
            value.as_bytes(),
        ).map_err(|e| err("set_raw_value_by", e))?;

        let mut file = std::fs::File::create(&config_path).map_err(|e| err("create config file", e))?;
        config.write_to(&mut file).map_err(|e| err("write config", e))?;
        Ok(())
    }

    /// Shorthand names of all local branches.
    #[napi]
    pub fn branch_names(&self) -> Result<Vec<String>> {
        let r = thread_local(&self.inner);
        let platform = r.references().map_err(|e| err("references", e))?;
        let mut out = Vec::new();
        for reference in platform.all().map_err(|e| err("references iter", e))? {
            let reference = reference.map_err(|e| err("reference", e))?;
            let name = lossy(reference.name().as_bstr());
            if let Some(shorthand) = name.strip_prefix("refs/heads/") {
                out.push(shorthand.to_string());
            }
        }
        Ok(out)
    }

    /// The remote name configured for a branch.
    #[napi]
    pub fn branch_remote_name(&self, branch: String) -> Option<String> {
        let key = format!("branch.{branch}.remote");
        self.config_string(key)
    }

    /// The remote ref configured for a branch.
    #[napi]
    pub fn branch_remote_ref(&self, branch: String) -> Option<String> {
        let key = format!("branch.{branch}.merge");
        self.config_string(key)
    }

    /// Whether `ancestor` commit is an ancestor of `descendant` commit.
    #[napi]
    pub fn is_ancestor(&self, ancestor: String, descendant: String) -> Result<bool> {
        let r = thread_local(&self.inner);
        let anc = parse_oid(&ancestor)?;
        let desc = parse_oid(&descendant)?;
        if anc == desc {
            return Ok(true);
        }
        let base = r.merge_base(anc, desc).map_err(|e| err("is_ancestor", e))?;
        Ok(base.map(|b| b.detach()) == Some(anc))
    }

    /// Describe a commit or HEAD (similar to `git describe`).
    #[napi]
    pub fn describe(&self, rev: Option<String>) -> Result<String> {
        let r = thread_local(&self.inner);
        let commit = match rev {
            Some(s) => r.find_commit(parse_oid(&s)?).map_err(|e| err("describe find_commit", e))?,
            None => r.head_commit().map_err(|e| err("describe head_commit", e))?,
        };
        let mut desc = commit.describe();
        let fmt = desc.format().map_err(|e| err("describe format", e))?;
        Ok(fmt.to_string())
    }

    /// Return whether the repository is shallow.
    #[napi]
    pub fn is_shallow(&self) -> bool {
        let r = thread_local(&self.inner);
        r.is_shallow().unwrap_or(false)
    }

    // ---- index & status ---------------------------------------------------

    /// Read entries from the repository index.
    #[napi]
    pub fn index(&self) -> Result<Vec<IndexEntry>> {
        let r = thread_local(&self.inner);
        let index = r.index().map_err(|e| err("index", e))?;
        let mut out = Vec::new();
        for entry in index.entries() {
            let path = lossy(entry.path(&index).as_ref());
            out.push(IndexEntry {
                path,
                mode: format!("{:?}", entry.mode),
                id: entry.id.to_string(),
                stage: entry.stage() as i32,
                size: entry.stat.size,
            });
        }
        Ok(out)
    }

    /// Read entries from the repository index (alias for `index`).
    #[napi]
    pub fn index_or_load(&self) -> Result<Vec<IndexEntry>> {
        self.index()
    }

    /// Add or update a file path in the index and return the blob OID.
    #[napi]
    pub fn edit_index(&self, path: String) -> Result<String> {
        let r = thread_local(&self.inner);
        let workdir = r.workdir().ok_or_else(|| err("edit_index", "bare repository"))?;
        let full_path = workdir.join(&path);
        let content = std::fs::read(&full_path).map_err(|e| err("read file for edit_index", e))?;
        let oid = r.write_blob(content).map_err(|e| err("write_blob for edit_index", e))?;
        let index_ref = r.index().map_err(|e| err("read index", e))?;
        let mut index = (*index_ref).clone();
        use gix::bstr::ByteSlice;
        index.dangerously_push_entry(
            gix::index::entry::Stat::default(),
            oid.detach(),
            gix::index::entry::Flags::empty(),
            gix::index::entry::Mode::FILE,
            path.as_bytes().as_bstr(),
        );
        let mut file = std::fs::File::create(r.git_dir().join("index")).map_err(|e| err("create index file", e))?;
        index.write_to(&mut file, gix::index::write::Options::default()).map_err(|e| err("write index", e))?;
        Ok(oid.to_string())
    }

    /// Asynchronously check repository status against the worktree and index.
    #[napi(ts_return_type = "Promise<Array<StatusEntry>>")]
    pub fn status(&self) -> Result<AsyncTask<StatusTask>> {
        Ok(AsyncTask::new(StatusTask {
            repo: Arc::clone(&self.inner),
        }))
    }

    /// Check index-worktree status synchronously.
    #[napi]
    pub fn index_worktree(&self) -> Result<Vec<StatusEntry>> {
        let r = thread_local(&self.inner);
        let status = r.status(gix::progress::Discard).map_err(|e| err("status", e))?;
        let iter = status.into_index_worktree_iter(None).map_err(|e| err("status iter", e))?;
        let mut out = Vec::new();
        for entry in iter {
            let item = entry.map_err(|e| err("status item", e))?;
            let path = lossy(item.rela_path().as_ref());
            let status_str = format!("{:?}", item);
            out.push(StatusEntry {
                path,
                status: status_str.clone(),
                index_status: None,
                worktree_status: Some(status_str),
            });
        }
        Ok(out)
    }

    // ---- object writing & raw access ------------------------------------

    /// Try to find an object of any kind, returning null if not found.
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

    /// Resolve a revision specification to a 40-character hex SHA string.
    #[napi]
    pub fn rev_parse_hex(&self, spec: String) -> Result<String> {
        self.rev_parse_single(spec)
    }

    /// Write a commit object directly into the ODB without updating any reference.
    #[napi]
    pub fn write_commit(
        &self,
        tree: String,
        parents: Vec<String>,
        author: Option<SignatureInput>,
        committer: Option<SignatureInput>,
        message: String,
    ) -> Result<String> {
        let r = thread_local(&self.inner);
        let tree_id = parse_oid(&tree)?;
        let parent_ids: Vec<gix::ObjectId> = parents
            .iter()
            .map(|p| parse_oid(p))
            .collect::<Result<Vec<_>>>()?;

        let fallback_sig = gix::actor::Signature {
            name: "Unknown".into(),
            email: "unknown@example.com".into(),
            time: gix::date::Time::new(0, 0),
        };
        let author_sig = match author {
            Some(ref a) => parse_signature(a),
            None => match r.author() {
                Some(Ok(sig)) => sig.to_owned().unwrap_or_else(|_| fallback_sig.clone()),
                _ => fallback_sig.clone(),
            },
        };
        let committer_sig = match committer {
            Some(ref c) => parse_signature(c),
            None => match r.committer() {
                Some(Ok(sig)) => sig.to_owned().unwrap_or_else(|_| author_sig.clone()),
                _ => author_sig.clone(),
            },
        };

        let commit = gix::objs::Commit {
            tree: tree_id,
            parents: parent_ids.into(),
            author: author_sig,
            committer: committer_sig,
            encoding: None,
            message: message.into(),
            extra_headers: Vec::new(),
        };

        let id = r.write_object(commit).map_err(|e| err("write_commit", e))?;
        Ok(id.to_string())
    }

    /// Write a tree object directly into the ODB given an array of tree items.
    #[napi]
    pub fn write_tree(&self, entries: Vec<TreeItemInput>) -> Result<String> {
        let r = thread_local(&self.inner);
        let mut tree_entries = Vec::with_capacity(entries.len());
        for entry in entries {
            let mode = parse_tree_mode(&entry.mode)?;
            let oid = parse_oid(&entry.id)?;
            tree_entries.push(gix::objs::tree::Entry {
                mode,
                filename: entry.name.into(),
                oid,
            });
        }
        let mut tree = gix::objs::Tree { entries: tree_entries };
        tree.entries.sort();
        let id = r.write_object(tree).map_err(|e| err("write_tree", e))?;
        Ok(id.to_string())
    }

    /// Write an annotated tag object directly into the ODB.
    #[napi]
    pub fn write_tag(
        &self,
        name: String,
        target: String,
        target_kind: String,
        tagger: Option<SignatureInput>,
        message: String,
    ) -> Result<String> {
        let r = thread_local(&self.inner);
        let target_oid = parse_oid(&target)?;
        let kind = parse_object_kind(&target_kind)?;
        let tagger_sig = tagger.as_ref().map(parse_signature);

        let tag = gix::objs::Tag {
            target: target_oid,
            target_kind: kind,
            name: name.into(),
            tagger: tagger_sig,
            message: message.into(),
            signature: None,
        };

        let id = r.write_object(tag).map_err(|e| err("write_tag", e))?;
        Ok(id.to_string())
    }

    /// Write a raw object of `kind` (`commit`, `tree`, `blob`, `tag`) with raw byte content.
    #[napi]
    pub fn write_object(
        &self,
        kind: String,
        data: napi::bindgen_prelude::Buffer,
    ) -> Result<String> {
        let r = thread_local(&self.inner);
        let obj_kind = parse_object_kind(&kind)?;
        use gix::prelude::Write;
        let id = r
            .write_buf(obj_kind, &data)
            .map_err(|e| err("write_object", e))?;
        Ok(id.to_string())
    }

    // ---- merge / rebase / cherry-pick / apply ----------------------------

    /// Merge two trees (with optional ancestor tree).
    #[napi]
    pub fn merge_trees(
        &self,
        our_tree: String,
        their_tree: String,
        ancestor_tree: Option<String>,
    ) -> Result<MergeOutcome> {
        let r = thread_local(&self.inner);
        let ours = parse_oid(&our_tree)?;
        let theirs = parse_oid(&their_tree)?;
        let ancestor = match ancestor_tree {
            Some(a) => parse_oid(&a)?,
            None => gix::ObjectId::empty_tree(r.object_hash()),
        };

        let labels = gix::merge::blob::builtin_driver::text::Labels::default();
        let mut outcome = r
            .merge_trees(ancestor, ours, theirs, labels, gix::merge::tree::Options::default())
            .map_err(|e| err("merge_trees", e))?;

        let has_conflicts = !outcome.conflicts.is_empty();
        let mut conflicts = Vec::new();
        for c in &outcome.conflicts {
            conflicts.push(format!("{:?}", c));
        }
        let tree_id = outcome.tree.write().ok().map(|id| id.to_string());

        Ok(MergeOutcome {
            tree_id,
            conflicts,
            has_conflicts,
        })
    }

    /// Merge two commits by finding their merge base and merging their trees.
    #[napi]
    pub fn merge_commits(&self, our_commit: String, their_commit: String) -> Result<MergeOutcome> {
        let r = thread_local(&self.inner);
        let our_c = r.find_commit(parse_oid(&our_commit)?).map_err(|e| err("find our_commit", e))?;
        let their_c = r.find_commit(parse_oid(&their_commit)?).map_err(|e| err("find their_commit", e))?;

        let base_id = r.merge_base(our_c.id, their_c.id).map_err(|e| err("merge_base", e))?;
        let ancestor_tree = match base_id {
            Some(bid) => Some(r.find_commit(bid).map_err(|e| err("find base commit", e))?.tree_id().map_err(|e| err("tree_id", e))?.to_string()),
            None => None,
        };

        self.merge_trees(our_c.tree_id().map_err(|e| err("our tree_id", e))?.to_string(), their_c.tree_id().map_err(|e| err("their tree_id", e))?.to_string(), ancestor_tree)
    }

    /// Cherry-pick a commit onto `onto` (or HEAD) and create a new commit.
    #[napi]
    pub fn cherry_pick(&self, commit: String, onto: Option<String>) -> Result<String> {
        let r = thread_local(&self.inner);
        let commit_obj = r.find_commit(parse_oid(&commit)?).map_err(|e| err("find commit", e))?;
        let parent_id = commit_obj.parent_ids().next().ok_or_else(|| err("cherry_pick", "commit has no parent"))?.detach();

        let target_commit = match onto {
            Some(o) => parse_oid(&o)?,
            None => r.head_id().map_err(|e| err("head_id", e))?.into(),
        };

        let target_obj = r.find_commit(target_commit).map_err(|e| err("find target_commit", e))?;
        let parent_obj = r.find_commit(parent_id).map_err(|e| err("find parent_commit", e))?;

        let outcome = self.merge_trees(
            target_obj.tree_id().map_err(|e| err("target tree_id", e))?.to_string(),
            commit_obj.tree_id().map_err(|e| err("commit tree_id", e))?.to_string(),
            Some(parent_obj.tree_id().map_err(|e| err("parent tree_id", e))?.to_string()),
        )?;

        if outcome.has_conflicts || outcome.tree_id.is_none() {
            return Err(err("cherry_pick", "merge conflict during cherry-pick"));
        }

        let new_tree = outcome.tree_id.unwrap();
        let message = lossy(commit_obj.message_raw().map_err(|e| err("commit message", e))?);

        self.write_commit(new_tree, vec![target_commit.to_string()], None, None, message)
    }

    /// Rebase commits from `upstream` to `branch` (or HEAD) onto `upstream`.
    #[napi]
    pub fn rebase(&self, upstream: String, branch: Option<String>) -> Result<RebaseOutcome> {
        let r = thread_local(&self.inner);
        let upstream_id = parse_oid(&upstream)?;
        let head: gix::ObjectId = match branch {
            Some(b) => r.find_reference(b.as_str()).map_err(|e| err("find branch", e))?.peel_to_id().map_err(|e| err("peel branch", e))?.detach(),
            None => r.head_id().map_err(|e| err("head_id", e))?.into(),
        };

        let revs = r.rev_walk([head]).all().map_err(|e| err("rev_walk", e))?;
        let mut commits_to_apply = Vec::new();
        for rev in revs {
            let item = rev.map_err(|e| err("rev item", e))?;
            if item.id == upstream_id {
                break;
            }
            commits_to_apply.push(item.id.to_string());
        }
        commits_to_apply.reverse();

        let mut current_target = upstream_id.to_string();
        let mut applied = Vec::new();
        for c in &commits_to_apply {
            let new_c = self.cherry_pick(c.clone(), Some(current_target))?;
            current_target = new_c.clone();
            applied.push(new_c);
        }

        Ok(RebaseOutcome {
            head_id: current_target,
            applied_commits: applied,
            has_conflicts: false,
        })
    }

    /// Apply a patch string to the repository worktree or index using `git apply`.
    #[napi]
    pub fn apply(&self, patch: String, index_only: Option<bool>) -> Result<bool> {
        let r = thread_local(&self.inner);
        let mut cmd = std::process::Command::new("git");
        if let Some(workdir) = r.workdir() {
            cmd.current_dir(workdir);
        } else {
            cmd.current_dir(r.git_dir());
        }
        cmd.arg("apply");
        if index_only.unwrap_or(false) {
            cmd.arg("--cached");
        }
        cmd.arg("-");

        use std::io::Write;
        let mut child = cmd.stdin(std::process::Stdio::piped()).spawn().map_err(|e| err("spawn git apply", e))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(patch.as_bytes()).map_err(|e| err("write patch stdin", e))?;
        }
        let status = child.wait().map_err(|e| err("wait git apply", e))?;
        Ok(status.success())
    }

    // ---- attributes / excludes / mailmap ----------------------------------

    /// Check if `path` is ignored by `.gitignore` / exclude rules.
    #[napi]
    pub fn is_path_ignored(&self, path: String) -> Result<bool> {
        let r = thread_local(&self.inner);
        let index = r.index().map_err(|e| err("index", e))?;
        let mut stack = r.excludes(
            &index,
            None,
            Default::default(),
        ).map_err(|e| err("excludes", e))?;

        let platform = stack.at_path(&path, Some(gix::object::tree::EntryKind::Blob.into())).map_err(|e| err("at_path", e))?;
        Ok(platform.is_excluded())
    }

    /// Read attributes for `path`.
    #[napi]
    pub fn attributes(&self, path: String) -> Result<Vec<AttributeEntry>> {
        let r = thread_local(&self.inner);
        let index = r.index().map_err(|e| err("index", e))?;
        let mut stack = r.attributes(
            &index,
            Default::default(),
            Default::default(),
            None,
        ).map_err(|e| err("attributes", e))?;

        let platform = stack.at_path(&path, Some(gix::object::tree::EntryKind::Blob.into())).map_err(|e| err("at_path", e))?;
        let mut outcome = gix::attrs::search::Outcome::default();
        platform.matching_attributes(&mut outcome);
        let mut out = Vec::new();
        for item in outcome.iter() {
            out.push(AttributeEntry {
                name: item.assignment.name.as_str().to_string(),
                value: format!("{:?}", item.assignment),
            });
        }
        Ok(out)
    }

    /// Filter pipeline settings for `path`.
    #[napi]
    pub fn filter_pipeline(&self, path: String) -> Result<Vec<String>> {
        let attrs = self.attributes(path)?;
        let mut filters = Vec::new();
        for attr in attrs {
            if attr.name == "filter" || attr.name == "text" || attr.name == "eol" {
                filters.push(format!("{}={}", attr.name, attr.value));
            }
        }
        Ok(filters)
    }

    /// Resolve author/committer name and email through repository `.mailmap`.
    #[napi]
    pub fn resolve_mailmap(&self, name: String, email: String) -> Result<MailmapEntry> {
        let r = thread_local(&self.inner);
        let mailmap = r.open_mailmap();
        let sig = gix::actor::SignatureRef {
            name: name.as_bytes().into(),
            email: email.as_bytes().into(),
            time: "0 +0000",
        };
        let resolved = mailmap.resolve(sig);
        Ok(MailmapEntry {
            name: Some(name.clone()),
            email: email.clone(),
            proper_name: Some(lossy(resolved.name.as_ref())),
            proper_email: Some(lossy(resolved.email.as_ref())),
        })
    }

    // ---- submodules, blame, notes, state, checkout, reset -----------------

    /// List submodules in this repository.
    #[napi]
    pub fn submodules(&self) -> Result<Vec<SubmoduleInfo>> {
        let r = thread_local(&self.inner);
        let mut out = Vec::new();
        if let Some(sms) = r.submodules().map_err(|e| err("submodules", e))? {
            for sm in sms {
                let path = sm.path().map(|p| lossy(p.as_ref())).unwrap_or_default();
                let url = sm.url().ok().map(|u| lossy(u.to_bstring().as_ref()));
                out.push(SubmoduleInfo {
                    name: lossy(sm.name().as_ref()),
                    path,
                    url,
                });
            }
        }
        Ok(out)
    }

    /// Read blame/annotation hunks for `path`.
    #[napi]
    pub fn blame(&self, path: String, start_commit: Option<String>) -> Result<Vec<BlameHunk>> {
        let r = thread_local(&self.inner);
        let commit = match start_commit {
            Some(c) => parse_oid(&c)?,
            None => r.head_id().map_err(|e| err("head_id", e))?.into(),
        };
        use gix::bstr::ByteSlice;
        let mut out = Vec::new();
        if let Ok(outcome) = r.blame_file(path.as_bytes().as_bstr(), commit, Default::default()) {
            for entry in outcome.entries {
                let author = r.find_commit(entry.commit_id).ok().and_then(|c| {
                    let a = c.author().ok()?;
                    Some((lossy(a.name), lossy(a.email)))
                });
                let (author_name, author_email) = author.unwrap_or_default();
                out.push(BlameHunk {
                    commit_id: entry.commit_id.to_string(),
                    start_line: entry.start_in_source_file as u32,
                    line_count: entry.len.get() as u32,
                    author_name,
                    author_email,
                });
            }
        }
        Ok(out)
    }

    /// Read git notes from `ref_name` (or `refs/notes/commits`).
    #[napi]
    pub fn notes(&self, ref_name: Option<String>) -> Result<Vec<NoteEntry>> {
        let r = thread_local(&self.inner);
        let note_ref = ref_name.unwrap_or_else(|| "refs/notes/commits".to_string());
        let mut out = Vec::new();
        if let Ok(mut ref_obj) = r.find_reference(&note_ref) {
            if let Ok(commit_id) = ref_obj.peel_to_id() {
                if let Ok(commit) = r.find_commit(commit_id) {
                    if let Ok(tree) = commit.tree() {
                        for entry in tree.iter().flatten() {
                            out.push(NoteEntry {
                                annotated_id: lossy(entry.filename().as_ref()),
                                note_id: entry.id().to_string(),
                            });
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// Return repository state string (e.g. `None`, `Merge`, `Rebase`, `CherryPick`, `Bisect`).
    #[napi]
    pub fn state(&self) -> String {
        let r = thread_local(&self.inner);
        format!("{:?}", r.state())
    }

    /// Get configured default author signature.
    #[napi]
    pub fn author(&self) -> Option<crate::object::Signature> {
        let r = thread_local(&self.inner);
        let sig = r.author().and_then(|res| res.ok())?;
        let t = sig.time().ok()?;
        Some(crate::object::Signature {
            name: lossy(sig.name),
            email: lossy(sig.email),
            time: t.seconds,
            timezone_offset: t.offset,
        })
    }

    /// Get configured default committer signature.
    #[napi]
    pub fn committer(&self) -> Option<crate::object::Signature> {
        let r = thread_local(&self.inner);
        let sig = r.committer().and_then(|res| res.ok())?;
        let t = sig.time().ok()?;
        Some(crate::object::Signature {
            name: lossy(sig.name),
            email: lossy(sig.email),
            time: t.seconds,
            timezone_offset: t.offset,
        })
    }

    /// Checkout a reference or commit into the working directory using `git checkout`.
    #[napi]
    pub fn checkout(&self, ref_or_commit: String) -> Result<bool> {
        let r = thread_local(&self.inner);
        let mut cmd = std::process::Command::new("git");
        if let Some(workdir) = r.workdir() {
            cmd.current_dir(workdir);
        } else {
            cmd.current_dir(r.git_dir());
        }
        cmd.args(["checkout", &ref_or_commit]);
        let status = cmd.status().map_err(|e| err("checkout", e))?;
        Ok(status.success())
    }

    /// Reset repository HEAD and index/worktree to `commit` using `git reset`.
    #[napi]
    pub fn reset(&self, commit: String, hard: Option<bool>) -> Result<bool> {
        let r = thread_local(&self.inner);
        let mut cmd = std::process::Command::new("git");
        if let Some(workdir) = r.workdir() {
            cmd.current_dir(workdir);
        } else {
            cmd.current_dir(r.git_dir());
        }
        cmd.arg("reset");
        if hard.unwrap_or(false) {
            cmd.arg("--hard");
        }
        cmd.arg(&commit);
        let status = cmd.status().map_err(|e| err("reset", e))?;
        Ok(status.success())
    }

    // ---- worktrees --------------------------------------------------------

    /// Return info for the main worktree, if any.
    #[napi]
    pub fn main_worktree(&self) -> Option<WorktreeInfo> {
        let r = thread_local(&self.inner);
        let work_dir = r.workdir()?;
        Some(WorktreeInfo {
            name: None,
            path: work_dir.to_string_lossy().into_owned(),
            git_dir: r.git_dir().to_string_lossy().into_owned(),
            is_main: true,
            is_locked: false,
        })
    }

    /// Return info for the current worktree.
    #[napi]
    pub fn worktree(&self) -> Option<WorktreeInfo> {
        let r = thread_local(&self.inner);
        let work_dir = r.workdir()?;
        let wt = r.worktree();
        let id = wt.as_ref().and_then(|w| w.id().ok().flatten().map(|i| lossy(i.as_ref())));
        let is_main = id.is_none();
        Some(WorktreeInfo {
            name: id,
            path: work_dir.to_string_lossy().into_owned(),
            git_dir: r.git_dir().to_string_lossy().into_owned(),
            is_main,
            is_locked: wt.as_ref().map(|w| w.is_locked()).unwrap_or(false),
        })
    }

    /// List all worktrees for this repository (including the main worktree if present).
    #[napi]
    pub fn worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        let r = thread_local(&self.inner);
        let mut out = Vec::new();
        if let Some(main) = self.main_worktree() {
            out.push(main);
        }
        if let Ok(proxy_list) = r.worktrees() {
            for proxy in proxy_list {
                let id = proxy.id().ok().map(|i| lossy(&*i));
                let base = proxy.base().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                out.push(WorktreeInfo {
                    name: id,
                    path: base,
                    git_dir: proxy.git_dir().to_string_lossy().into_owned(),
                    is_main: false,
                    is_locked: proxy.is_locked(),
                });
            }
        }
        Ok(out)
    }

    /// Create a new linked worktree at `path` with `name`.
    #[napi]
    pub fn create_worktree(&self, name: String, path: String) -> Result<Repository> {
        let r = thread_local(&self.inner);
        let mut cmd = std::process::Command::new("git");
        cmd.current_dir(r.git_dir());
        cmd.args(["worktree", "add", "-b", &name, &path]);
        let status = cmd.status().map_err(|e| err("create_worktree git command", e))?;
        if !status.success() {
            return Err(err("create_worktree", "git worktree add command failed"));
        }
        Repository::open(path)
    }

    /// Whether this repository instance is a linked worktree.
    #[napi]
    pub fn is_worktree(&self) -> bool {
        let r = thread_local(&self.inner);
        matches!(r.kind(), gix::repository::Kind::LinkedWorkTree { .. })
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
