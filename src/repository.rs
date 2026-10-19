use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use napi::Result;
use napi::Task;
use napi::bindgen_prelude::AsyncTask;
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};
use crate::object::{
    Blob, Commit, GitObject, Tag, Tree, build_blob, build_commit, build_object, build_tag,
    build_tree,
};
use crate::reference::{Head, Reference, build_head, build_reference};
use crate::remote::Remote;
use crate::remote::build_remote;

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
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

/// Header metadata of a git object.
#[derive(Clone, Debug)]
#[napi(object)]
pub struct ObjectHeader {
    pub kind: String,
    pub size: f64,
}

/// A single entry input for writing a tree object.
#[derive(Clone, Debug)]
#[napi(object)]
pub struct TreeItemInput {
    pub name: String,
    pub id: String,
    /// Git mode string, e.g. `"100644"`, `"100755"`, `"040000"`, `"120000"`.
    pub mode: String,
}

/// A key-value config entry.
#[derive(Clone, Debug)]
#[napi(object)]
pub struct ConfigEntry {
    pub key: String,
    pub value: String,
}

/// An entry in the git index / staging area.
#[derive(Clone, Debug)]
#[napi(object)]
pub struct IndexEntry {
    pub path: String,
    pub id: String,
    pub mode: String,
    pub size: f64,
}

/// Information about a git worktree.
#[derive(Clone, Debug)]
#[napi(object)]
pub struct WorktreeInfo {
    pub id: Option<String>,
    pub git_dir: String,
    pub work_dir: String,
    pub is_main: bool,
    pub is_locked: bool,
}

/// Resolved author or committer identity from mailmap.
#[derive(Clone, Debug)]
#[napi(object)]
pub struct MailmapEntry {
    pub name: String,
    pub email: String,
}

/// Submodule information.
#[derive(Clone, Debug)]
#[napi(object)]
pub struct SubmoduleInfo {
    pub name: String,
    pub path: String,
    pub url: Option<String>,
}

/// Signature identity (author or committer).
#[derive(Clone, Debug)]
#[napi(object)]
pub struct SignatureInfo {
    pub name: String,
    pub email: String,
    pub time_seconds: i64,
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
        let ts = gix::ThreadSafeRepository::open(path).map_err(|e| err("open", e))?;
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
        build_commit(
            &self.inner,
            r.head_commit().map_err(|e| err("head_commit", e))?,
        )
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
        Ok(r.head_name()
            .map_err(|e| err("head_name", e))?
            .map(|n| n.to_string()))
    }

    /// Return the tree object ID that `HEAD` points to.
    #[napi]
    pub fn head_tree_id(&self) -> Result<String> {
        let r = thread_local(&self.inner);
        let id = r.head_tree_id().map_err(|e| err("head_tree_id", e))?;
        Ok(id.to_string())
    }

    /// Return the reference `HEAD` points to, or null if detached/unborn.
    #[napi]
    pub fn head_ref(&self) -> Result<Option<Reference>> {
        let r = thread_local(&self.inner);
        let head_ref = r.head_ref().map_err(|e| err("head_ref", e))?;
        match head_ref {
            Some(rf) => Ok(Some(build_reference(&self.inner, rf)?)),
            None => Ok(None),
        }
    }

    /// Return the tree object that `HEAD` points to.
    #[napi]
    pub fn head_tree(&self) -> Result<Tree> {
        let r = thread_local(&self.inner);
        let tree_id = r.head_tree_id().map_err(|e| err("head_tree_id", e))?;
        build_tree(&self.inner, tree_id)
    }

    /// Set `HEAD` to point to a reference name (e.g. `"refs/heads/main"`) or detach to an object ID.
    #[napi]
    pub fn set_head(&self, name_or_target: String) -> Result<()> {
        let mut r = thread_local(&self.inner);
        let git_dir = r.git_dir().to_path_buf();
        let head_file = git_dir.join("HEAD");
        if let Ok(oid) = parse_oid(&name_or_target) {
            std::fs::write(&head_file, format!("{oid}\n"))
                .map_err(|e| napi::Error::from_reason(format!("failed to write HEAD file: {e}")))?;
        } else {
            let ref_name = if name_or_target.starts_with("refs/") {
                name_or_target
            } else {
                format!("refs/heads/{name_or_target}")
            };
            std::fs::write(&head_file, format!("ref: {ref_name}\n"))
                .map_err(|e| napi::Error::from_reason(format!("failed to write HEAD file: {e}")))?;
        }
        r.reload().map_err(|e| err("reload", e))?;
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

    /// Try to find an object by ID, returning null if not found.
    #[napi]
    pub fn try_find_object(&self, id: String) -> Result<Option<GitObject>> {
        let r = thread_local(&self.inner);
        let oid = match parse_oid(&id) {
            Ok(o) => o,
            Err(_) => return Ok(None),
        };
        match r
            .try_find_object(oid)
            .map_err(|e| err("try_find_object", e))?
        {
            Some(obj) => Ok(Some(build_object(&self.inner, obj)?)),
            None => Ok(None),
        }
    }

    /// Obtain header information about an object without fully decoding it.
    #[napi]
    pub fn try_find_header(&self, id: String) -> Result<Option<ObjectHeader>> {
        let r = thread_local(&self.inner);
        let oid = match parse_oid(&id) {
            Ok(o) => o,
            Err(_) => return Ok(None),
        };
        match r
            .try_find_header(oid)
            .map_err(|e| err("try_find_header", e))?
        {
            Some(header) => Ok(Some(ObjectHeader {
                kind: format!("{:?}", header.kind()).to_lowercase(),
                size: header.size() as f64,
            })),
            None => Ok(None),
        }
    }

    /// Create and write a tree object from entries and return its object ID.
    #[napi]
    pub fn write_tree(&self, entries: Vec<TreeItemInput>) -> Result<String> {
        let r = thread_local(&self.inner);
        let mut editor = r.empty_tree().edit().map_err(|e| err("edit_tree", e))?;
        for item in entries {
            let mode = match item.mode.as_str() {
                "100755" | "0100755" | "Executable" | "BlobExecutable" => {
                    gix::object::tree::EntryKind::BlobExecutable
                }
                "040000" | "0040000" | "Tree" => gix::object::tree::EntryKind::Tree,
                "120000" | "0120000" | "Link" | "Symlink" => gix::object::tree::EntryKind::Link,
                _ => gix::object::tree::EntryKind::Blob,
            };
            editor
                .upsert(item.name.as_str(), mode, parse_oid(&item.id)?)
                .map_err(|e| err("upsert_tree_entry", e))?;
        }
        let tree_id = editor.write().map_err(|e| err("write_tree", e))?;
        Ok(tree_id.to_string())
    }

    /// Resolve a revision specification to a hex object ID string.
    #[napi]
    pub fn rev_parse_hex(&self, spec: String) -> Result<String> {
        self.rev_parse_single(spec)
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

    /// Return the empty tree ID for this repository's object hash.
    #[napi]
    pub fn empty_tree_id(&self) -> String {
        let r = thread_local(&self.inner);
        r.empty_tree().id.to_string()
    }

    // ---- references & reflogs ----------------------------------------------

    /// Read reflog entries for a reference (e.g. `"refs/heads/main"`, `"HEAD"`).
    ///
    /// Defaults to `"HEAD"` if `name` is omitted.
    #[napi]
    pub fn reflog(&self, name: Option<String>) -> Result<Vec<crate::reference::ReflogEntry>> {
        let ref_name = name.unwrap_or_else(|| "HEAD".to_string());
        let reference = self.find_reference(ref_name)?;
        reference.log()
    }

    /// Read reflog entries for `HEAD`.
    #[napi]
    pub fn head_reflog(&self) -> Result<Vec<crate::reference::ReflogEntry>> {
        self.reflog(Some("HEAD".to_string()))
    }

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

    /// List local branch names.
    #[napi]
    pub fn branch_names(&self) -> Result<Vec<String>> {
        let r = thread_local(&self.inner);
        let platform = r.references().map_err(|e| err("references", e))?;
        let mut names = Vec::new();
        if let Ok(local) = platform.local_branches() {
            for rf in local {
                if let Ok(rf) = rf {
                    names.push(rf.name().shorten().to_string());
                }
            }
        }
        Ok(names)
    }

    /// Get remote name configured for a local branch.
    #[napi]
    pub fn branch_remote_name(&self, branch_name: String) -> Result<Option<String>> {
        let key = format!("branch.{branch_name}.remote");
        Ok(self.config_string(key))
    }

    /// Delete local branch references.
    #[napi]
    pub fn delete_local_branches(&self, names: Vec<String>) -> Result<Vec<String>> {
        let r = thread_local(&self.inner);
        let mut deleted = Vec::new();
        for name in names {
            let ref_name = if name.starts_with("refs/heads/") {
                name.clone()
            } else {
                format!("refs/heads/{name}")
            };
            if let Ok(rf) = r.find_reference(&ref_name) {
                if rf.delete().is_ok() {
                    deleted.push(name);
                }
            }
        }
        Ok(deleted)
    }

    // ---- worktrees --------------------------------------------------------

    /// List all worktrees associated with this repository.
    #[napi]
    pub fn worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        let r = thread_local(&self.inner);
        let proxies = r.worktrees().map_err(|e| err("worktrees", e))?;
        let mut out = Vec::new();
        for proxy in proxies {
            let id = proxy.id().ok().map(|i| lossy(i.as_ref()));
            let git_dir = proxy.git_dir().to_string_lossy().into_owned();
            let work_dir = match proxy.base() {
                Ok(p) => p.to_string_lossy().into_owned(),
                Err(_) => String::new(),
            };
            let is_main = id.is_none();
            let is_locked = proxy.is_locked();
            out.push(WorktreeInfo {
                id,
                git_dir,
                work_dir,
                is_main,
                is_locked,
            });
        }

        // If no linked worktrees proxies found, synthesize worktree info for current repo if not bare
        if out.is_empty() && !r.is_bare() {
            if let Some(wd) = r.workdir() {
                out.push(WorktreeInfo {
                    id: None,
                    git_dir: r.git_dir().to_string_lossy().into_owned(),
                    work_dir: wd.to_string_lossy().into_owned(),
                    is_main: true,
                    is_locked: false,
                });
            }
        }
        Ok(out)
    }

    /// Check whether this repository is currently a worktree (linked or main).
    #[napi]
    pub fn is_worktree(&self) -> bool {
        let r = thread_local(&self.inner);
        r.workdir().is_some()
    }

    /// Get information about the main worktree directory if available.
    #[napi]
    pub fn main_worktree(&self) -> Result<Option<WorktreeInfo>> {
        let wts = self.worktrees()?;
        Ok(wts.into_iter().find(|wt| wt.is_main))
    }

    // ---- submodules & state ------------------------------------------------

    /// List submodules in this repository.
    #[napi]
    pub fn submodules(&self) -> Result<Vec<SubmoduleInfo>> {
        let r = thread_local(&self.inner);
        let mut out = Vec::new();
        if let Ok(m) = r.submodules() {
            if let Some(subs) = m {
                for sub in subs {
                    let name = lossy(sub.name());
                    let path = sub.path().map(|p| lossy(p.as_ref())).unwrap_or_default();
                    let url = match sub.url() {
                        Ok(u) => Some(u.to_bstring().to_string()),
                        Err(_) => None,
                    };
                    out.push(SubmoduleInfo { name, path, url });
                }
            }
        }
        Ok(out)
    }

    /// Return the in-progress operation state of the repository, e.g. `"merge"`, `"rebase"`, `"cherry-pick"`, or `"none"`.
    #[napi]
    pub fn state(&self) -> String {
        let r = thread_local(&self.inner);
        format!("{:?}", r.state()).to_lowercase()
    }

    /// Return the default committer signature for this repository.
    #[napi]
    pub fn committer(&self) -> Option<SignatureInfo> {
        let r = thread_local(&self.inner);
        r.committer().and_then(|res| res.ok()).map(|c| {
            let at = c.time().unwrap_or_else(|_| gix::date::Time::now_utc());
            SignatureInfo {
                name: lossy(c.name),
                email: lossy(c.email),
                time_seconds: at.seconds,
            }
        })
    }

    // ---- mailmap ----------------------------------------------------------

    /// Resolve author or committer name and email through repository `.mailmap`.
    #[napi]
    pub fn mailmap_resolve(&self, name: String, email: String) -> Result<MailmapEntry> {
        let r = thread_local(&self.inner);
        let mailmap = r.open_mailmap();
        let signature = gix::actor::SignatureRef {
            name: name.as_str().into(),
            email: email.as_str().into(),
            time: "0 +0000",
        };
        let resolved = mailmap.resolve(signature);
        Ok(MailmapEntry {
            name: lossy(&resolved.name),
            email: lossy(&resolved.email),
        })
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

    /// Find an in-memory remote by URL or name.
    #[napi]
    pub fn remote_at(&self, url: String) -> Result<Remote> {
        let r = thread_local(&self.inner);
        let remote = r.remote_at(url.as_str()).map_err(|e| err("remote_at", e))?;
        Ok(build_remote(&self.inner, remote))
    }

    /// Create and store a named remote in repository config.
    #[napi]
    pub fn create_remote(&mut self, name: String, url: String) -> Result<Remote> {
        self.set_config_value(format!("remote.{name}.url"), url)?;
        self.find_remote(name)
    }

    /// Read `repository.manifest.yml` and set up remotes for this repository.
    #[napi]
    pub fn setup_remotes_from_manifest(
        &self,
        manifest_path: Option<String>,
    ) -> Result<Vec<crate::util::RemoteInfo>> {
        crate::util::setup_remotes_from_manifest(self, manifest_path)
    }

    // ---- config / paths --------------------------------------------------

    /// Read a single config value as a string.
    #[napi]
    pub fn config_string(&self, key: String) -> Option<String> {
        let r = thread_local(&self.inner);
        let snapshot = r.config_snapshot();
        snapshot.string(key.as_str()).map(|v| lossy(&*v))
    }

    /// Return all key-value pairs from repository configuration snapshot.
    #[napi]
    pub fn config_entries(&self) -> Result<Vec<ConfigEntry>> {
        let r = thread_local(&self.inner);
        let config = r.config_snapshot();
        let mut entries = Vec::new();
        for section in config.sections() {
            let header = section.header();
            let section_name = lossy(header.name());
            let sub_name = header.subsection_name().map(|s| lossy(s));
            for value_name in section.value_names() {
                let name_str = lossy(value_name.as_ref());
                let full_key = match &sub_name {
                    Some(sub) => format!("{section_name}.{sub}.{name_str}"),
                    None => format!("{section_name}.{name_str}"),
                };
                if let Some(val) = section.value(value_name) {
                    entries.push(ConfigEntry {
                        key: full_key,
                        value: lossy(&*val),
                    });
                }
            }
        }
        Ok(entries)
    }

    /// Set a key-value pair in repository `.git/config`.
    #[napi]
    pub fn set_config_value(&mut self, key: String, value: String) -> Result<()> {
        let r = thread_local(&self.inner);
        let config_path = r.git_dir().join("config");
        let content = std::fs::read_to_string(&config_path).unwrap_or_default();
        let mut file = gix::config::File::from_bytes_no_includes(
            content.as_bytes(),
            gix::config::file::Metadata::api(),
            gix::config::file::init::Options::default(),
        )
        .map_err(|e| napi::Error::from_reason(format!("parse config file: {e}")))?;
        file.set_raw_value(key.as_str(), value.as_str())
            .map_err(|e| napi::Error::from_reason(format!("set config value: {e}")))?;
        let mut out = Vec::new();
        file.write_to(&mut out)
            .map_err(|e| napi::Error::from_reason(format!("write config file: {e}")))?;
        std::fs::write(&config_path, out)
            .map_err(|e| napi::Error::from_reason(format!("save config file: {e}")))?;

        // Re-open ThreadSafeRepository to refresh the config snapshot cache
        let git_dir = r.git_dir().to_path_buf();
        if let Ok(reloaded_ts) = gix::ThreadSafeRepository::open(git_dir) {
            self.inner = Arc::new(reloaded_ts);
        }
        Ok(())
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

    /// Whether this repository is shallow.
    #[napi]
    pub fn is_shallow(&self) -> bool {
        let r = thread_local(&self.inner);
        r.is_shallow().unwrap_or(false)
    }

    /// The repository kind as a string (`bare` / `working-tree`).
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

    // ---- index & worktree status ----------------------------------------

    /// Return all entries in the index/staging area.
    #[napi]
    pub fn index_entries(&self) -> Result<Vec<IndexEntry>> {
        let r = thread_local(&self.inner);
        let index = r.index_or_empty().map_err(|e| err("index", e))?;
        let mut out = Vec::new();
        for entry in index.entries() {
            let path = lossy(entry.path(&index));
            out.push(IndexEntry {
                path,
                id: entry.id.to_string(),
                mode: format!("{:o}", entry.mode.bits()),
                size: entry.stat.size as f64,
            });
        }
        Ok(out)
    }

    /// Check whether the working tree or index has uncommitted changes.
    #[napi]
    pub fn is_dirty(&self) -> Result<bool> {
        let r = thread_local(&self.inner);
        if r.is_bare() {
            return Ok(false);
        }
        let status = r.is_dirty();
        match status {
            Ok(dirty) => Ok(dirty),
            Err(_) => Ok(false),
        }
    }

    // ---- commits / revisions --------------------------------------------

    /// Check if `ancestor` commit is an ancestor of `descendant` commit.
    #[napi]
    pub fn is_ancestor(&self, ancestor: String, descendant: String) -> Result<bool> {
        let r = thread_local(&self.inner);
        let anc_id = parse_oid(&ancestor)?;
        let desc_id = parse_oid(&descendant)?;
        if anc_id == desc_id {
            return Ok(true);
        }
        let platform = r.rev_walk([desc_id]);
        for id in platform.all().map_err(|e| err("rev_walk", e))? {
            let id = id.map_err(|e| err("rev_walk item", e))?;
            if id.id == anc_id {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Describe a revision using nearby tags (equivalent to `git describe`).
    #[napi]
    pub fn describe(&self, rev: Option<String>) -> Result<String> {
        let r = thread_local(&self.inner);
        let oid = match rev {
            Some(spec) => parse_oid(
                &r.rev_parse_single(spec.as_str())
                    .map_err(|e| err("rev_parse_single", e))?
                    .to_string(),
            )?,
            None => r.head_id().map_err(|e| err("head_id", e))?.into(),
        };
        let commit = r.find_commit(oid).map_err(|e| err("find_commit", e))?;
        let outcome = commit.describe().format().map_err(|e| err("describe", e))?;
        Ok(outcome.to_string())
    }

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
            .commit(reference, message.as_str(), parse_oid(&tree)?, parent_ids)
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
                let (entry, parents) = match log_summary(&r, id) {
                    Ok(res) => res,
                    Err(_) => break, // Gracefully handle shallow repository boundaries
                };
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
            Some(o) => Some(
                r.find_tree(parse_oid(&o)?)
                    .map_err(|e| err("find_tree", e))?,
            ),
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
