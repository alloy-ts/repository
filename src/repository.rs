use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use napi::Result;
use napi::Task;
use napi::bindgen_prelude::AsyncTask;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;

use gix::progress::{Count, Id, MessageLevel, NestedProgress, Progress, Step, StepShared, Unit};

use gix::bstr::BString;
use gix::diff::index::Change as TreeIndexChange;
use gix::prelude::Write;
use gix::status::index_worktree::iter::Summary;

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

/// A single flattened git config entry: `key` (e.g. `remote.origin.url`) and its `value`.
#[napi(object)]
pub struct ConfigEntry {
    pub key: String,
    pub value: String,
}

/// A progress update delivered to JavaScript from a worker thread (e.g. during `clone`/`fetch`/
/// `push`/`checkout`/`status`). It is forwarded via a `ThreadsafeFunction` — the only safe way to
/// invoke a JS callback from a thread that is not the main Node.js thread.
#[napi(object)]
pub struct ProgressEvent {
    /// Current step within `max` (0 when unbounded).
    pub step: f64,
    /// Upper bound of the current step, if known.
    pub max: Option<f64>,
    /// The current task name, if set.
    pub name: Option<String>,
    /// A human-readable message emitted at this step, if any.
    pub message: Option<String>,
    /// Message level: `info`, `success`, or `failure`.
    pub level: Option<String>,
}

/// A single entry in the repository index (staging area).
#[napi(object)]
pub struct IndexEntry {
    /// Path relative to the repository root (e.g. `src/main.rs`).
    pub path: String,
    /// The entry mode as a string, e.g. `Blob`, `BlobExecutable`, `Symlink`, `Tree`.
    pub mode: String,
    /// The object id of the entry's blob/tree/commit.
    pub id: String,
    /// The stage: `0` (merged), `1` (base), `2` (ours), `3` (theirs).
    pub stage: u32,
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

/// A gix `Progress` sink that forwards each update to a JavaScript `ThreadsafeFunction`.
///
/// This is the only correct way to invoke a JS callback from a worker thread: `AsyncTask::compute`
/// runs off the main Node.js thread, where `napi_env`/`napi_value` do not exist, so we marshal the
/// update as an owned `ProgressEvent` and hand it to the thread-safe function, which queues the JS
/// call on the main thread's event loop.
struct TsfnProgress {
    tsfn: Option<Arc<ThreadsafeFunction<ProgressEvent, ()>>>,
    step: AtomicU64,
    max: Option<usize>,
    name: Option<String>,
}

impl TsfnProgress {
    fn emit(&self, message: Option<String>, level: Option<&str>) {
        if let Some(tsfn) = &self.tsfn {
            let ev = ProgressEvent {
                step: self.step.load(Ordering::SeqCst) as f64,
                max: self.max.map(|m| m as f64),
                name: self.name.clone(),
                message,
                level: level.map(|s| s.to_string()),
            };
            let _ = tsfn.call(Ok(ev), ThreadsafeFunctionCallMode::NonBlocking);
        }
    }
}

impl Count for TsfnProgress {
    fn set(&self, step: Step) {
        self.step.store(step as u64, Ordering::SeqCst);
        self.emit(None, None);
    }
    fn step(&self) -> Step {
        self.step.load(Ordering::SeqCst) as usize
    }
    fn inc_by(&self, step: Step) {
        self.set(self.step() + step);
    }
    fn counter(&self) -> StepShared {
        Arc::new(AtomicUsize::new(self.step()))
    }
}

impl Progress for TsfnProgress {
    fn init(&mut self, max: Option<Step>, _unit: Option<Unit>) {
        self.max = max;
    }
    fn set_name(&mut self, name: String) {
        self.name = Some(name);
    }
    fn name(&self) -> Option<String> {
        self.name.clone()
    }
    fn id(&self) -> Id {
        gix::progress::UNKNOWN
    }
    fn message(&self, level: MessageLevel, message: String) {
        let tag = match level {
            MessageLevel::Info => "info",
            MessageLevel::Success => "success",
            MessageLevel::Failure => "failure",
        };
        self.emit(Some(message), Some(tag));
    }
}

impl NestedProgress for TsfnProgress {
    type SubProgress = TsfnProgress;
    fn add_child(&mut self, name: impl Into<String>) -> TsfnProgress {
        let mut child = TsfnProgress {
            tsfn: self.tsfn.clone(),
            step: AtomicU64::new(0),
            max: None,
            name: None,
        };
        child.set_name(name.into());
        child
    }
    fn add_child_with_id(&mut self, name: impl Into<String>, _id: Id) -> TsfnProgress {
        self.add_child(name)
    }
}

/// A tree item input struct for writing custom tree objects via [`Repository::write_tree`].
#[napi(object)]
pub struct TreeItem {
    /// Relative path/filename of the entry inside the tree.
    pub path: String,
    /// Mode string, e.g. `"100644"`, `"100755"`, `"040000"`, `"120000"`, `"160000"`.
    pub mode: String,
    /// Object ID (hex hash) for the entry.
    pub id: String,
}

/// Input parameters for writing raw commit objects via [`Repository::write_commit`].
#[napi(object)]
pub struct WriteCommitInput {
    pub message: String,
    pub tree: String,
    pub parents: Vec<String>,
    pub author_name: Option<String>,
    pub author_email: Option<String>,
    pub committer_name: Option<String>,
    pub committer_email: Option<String>,
}

/// A single change reported by `status()` (working tree vs index, and index vs HEAD).
#[napi(object)]
pub struct StatusEntry {
    /// Repository-relative path of the changed entry.
    pub path: String,
    /// Change kind: `untracked`, `modified`, `deleted`, `renamed`, `copied`, `typechange`,
    /// `conflict`, `intent-to-add`, or `staged_added` / `staged_modified` / `staged_deleted`.
    pub kind: String,
}

/// Map a gix status `Summary` to a JS-friendly change kind.
fn summary_kind(s: Summary) -> String {
    match s {
        Summary::Conflict => "conflict",
        Summary::Removed => "deleted",
        Summary::TypeChange => "typechange",
        Summary::Modified => "modified",
        Summary::IntentToAdd => "intent-to-add",
        Summary::Added => "untracked",
        Summary::Copied => "copied",
        Summary::Renamed => "renamed",
    }
    .to_string()
}

/// Blocking clone work, executed on the libuv thread pool by [`AsyncTask`].
struct CloneTask {
    url: String,
    path: String,
    bare: bool,
    on_progress: Option<Arc<ThreadsafeFunction<ProgressEvent, ()>>>,
}

/// Blocking status walk, executed on the libuv thread pool by [`AsyncTask`].
struct StatusTask {
    inner: Arc<gix::ThreadSafeRepository>,
    on_progress: Option<Arc<ThreadsafeFunction<ProgressEvent, ()>>>,
}

impl Task for StatusTask {
    type Output = Vec<StatusEntry>;
    type JsValue = Vec<StatusEntry>;

    fn compute(&mut self) -> Result<Vec<StatusEntry>> {
        let r = thread_local(&self.inner);
        let progress = TsfnProgress {
            tsfn: self.on_progress.clone(),
            step: AtomicU64::new(0),
            max: None,
            name: None,
        };
        let mut iter = r
            .status(progress)
            .map_err(|e| err("status", e))?
            .into_iter(Vec::<BString>::new())
            .map_err(|e| err("status.iter", e))?;
        let mut out = Vec::new();
        while let Some(item) = iter.next() {
            let item = item.map_err(|e| err("status.item", e))?;
            let (path, kind) = match item {
                gix::status::Item::IndexWorktree(iw) => match iw.summary() {
                    Some(summary) => (lossy(iw.rela_path()), summary_kind(summary)),
                    None => continue,
                },
                gix::status::Item::TreeIndex(change) => {
                    let kind = match change {
                        TreeIndexChange::Addition { .. } => "staged_added",
                        TreeIndexChange::Deletion { .. } => "staged_deleted",
                        TreeIndexChange::Modification { .. } => "staged_modified",
                        TreeIndexChange::Rewrite { .. } => "staged_renamed",
                    };
                    (lossy(change.location()), kind.to_string())
                }
            };
            out.push(StatusEntry { path, kind });
        }
        Ok(out)
    }

    fn resolve(&mut self, _env: napi::Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
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
        let progress = TsfnProgress {
            tsfn: self.on_progress.take(),
            step: AtomicU64::new(0),
            max: None,
            name: None,
        };
        let (repo, _outcome) = prepare
            .fetch_only(progress, &interrupt)
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
    /// `Promise<Repository>`. If `on_progress` is provided, each progress update is streamed to it
    /// from the worker thread via a `ThreadsafeFunction` (the JS callback receives a `ProgressEvent`).
    #[napi(ts_return_type = "Promise<Repository>")]
    pub fn clone(
        url: String,
        path: String,
        bare: Option<bool>,
        on_progress: Option<ThreadsafeFunction<ProgressEvent, ()>>,
    ) -> Result<AsyncTask<CloneTask>> {
        Ok(AsyncTask::new(CloneTask {
            url,
            path,
            bare: bare.unwrap_or(false),
            on_progress: on_progress.map(Arc::new),
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

    /// The id of the tree `HEAD` points to (the root tree of the HEAD commit).
    #[napi]
    pub fn head_tree_id(&self) -> Result<String> {
        let r = thread_local(&self.inner);
        Ok(r
            .head_tree_id()
            .map_err(|e| err("head_tree_id", e))?
            .to_string())
    }

    /// The reference `HEAD` points to (e.g. `refs/heads/main`), or `null` if detached/unborn.
    #[napi]
    pub fn head_ref(&self) -> Result<Option<Reference>> {
        let r = thread_local(&self.inner);
        match r.head_ref().map_err(|e| err("head_ref", e))? {
            Some(rf) => Ok(Some(build_reference(&self.inner, rf)?)),
            None => Ok(None),
        }
    }

    /// The tree `HEAD` points to.
    #[napi]
    pub fn head_tree(&self) -> Result<Tree> {
        let r = thread_local(&self.inner);
        let tree = r.head_tree().map_err(|e| err("head_tree", e))?;
        build_tree(&self.inner, tree.id)
    }

    /// Point `HEAD` at `target`, which is either a commit id (detached) or a ref name
    /// (symbolic, e.g. `refs/heads/main` or `main`).
    #[napi]
    pub fn set_head(&self, target: String) -> Result<()> {
        let r = thread_local(&self.inner);
        let new_target: gix::refs::Target = if let Ok(oid) = parse_oid(&target) {
            gix::refs::Target::Object(oid)
        } else {
            gix::refs::Target::Symbolic(
                gix::refs::FullName::try_from(target.as_str())
                    .map_err(|e| err("set_head", e))?,
            )
        };
        let edit = gix::refs::transaction::RefEdit::update(
            gix::refs::FullName::try_from("HEAD").map_err(|e| err("set_head", e))?,
            new_target,
            gix::refs::transaction::PreviousValue::Any,
            "set head",
        );
        r.edit_reference(edit).map_err(|e| err("set_head", e))?;
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

    /// Try to find an object of any kind, returning `null` if not found instead of throwing.
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

    /// Parse a hex object ID string into its canonical 40-character (or 64-character) hex representation.
    #[napi]
    pub fn rev_parse_hex(&self, spec: String) -> Result<String> {
        let r = thread_local(&self.inner);
        let id = r
            .rev_parse_single(spec.as_str())
            .map_err(|e| err("rev_parse_hex", e))?;
        Ok(id.to_string())
    }

    /// Write a tree object from a collection of tree entries.
    #[napi]
    pub fn write_tree(&self, entries: Vec<TreeItem>) -> Result<String> {
        let r = thread_local(&self.inner);
        let mut tree = gix::objs::Tree::empty();
        for item in entries {
            let mode = match item.mode.as_str() {
                "040000" | "Tree" => gix::objs::tree::EntryKind::Tree,
                "100755" | "BlobExecutable" => gix::objs::tree::EntryKind::BlobExecutable,
                "120000" | "Link" | "Symlink" => gix::objs::tree::EntryKind::Link,
                "160000" | "Commit" => gix::objs::tree::EntryKind::Commit,
                _ => gix::objs::tree::EntryKind::Blob,
            };
            let oid = parse_oid(&item.id)?;
            tree.entries.push(gix::objs::tree::Entry {
                mode: mode.into(),
                filename: item.path.into(),
                oid,
            });
        }
        let id = r.write_object(&tree).map_err(|e| err("write_tree", e))?;
        Ok(id.to_string())
    }

    /// Write a raw commit object and return its id.
    #[napi]
    pub fn write_commit(&self, input: WriteCommitInput) -> Result<String> {
        let r = thread_local(&self.inner);
        let tree_id = parse_oid(&input.tree)?;
        let parent_ids = input
            .parents
            .iter()
            .map(|p| parse_oid(p))
            .collect::<Result<Vec<_>>>()?;

        let author_sig = match (input.author_name, input.author_email) {
            (Some(name), Some(email)) => gix::actor::Signature {
                name: name.into(),
                email: email.into(),
                time: gix::date::Time::now_utc(),
            },
            _ => match r.author() {
                Some(Ok(s)) => s.to_owned().map_err(|e| err("author.to_owned", e))?,
                _ => return Err(napi::Error::from_reason("no author configured")),
            },
        };

        let committer_sig = match (input.committer_name, input.committer_email) {
            (Some(name), Some(email)) => gix::actor::Signature {
                name: name.into(),
                email: email.into(),
                time: gix::date::Time::now_utc(),
            },
            _ => match r.committer() {
                Some(Ok(s)) => s.to_owned().map_err(|e| err("committer.to_owned", e))?,
                _ => return Err(napi::Error::from_reason("no committer configured")),
            },
        };

        let commit_obj = gix::objs::Commit {
            tree: tree_id,
            parents: parent_ids.into(),
            author: author_sig,
            committer: committer_sig,
            encoding: None,
            message: input.message.into(),
            extra_headers: Vec::new(),
        };

        let id = r.write_object(&commit_obj).map_err(|e| err("write_commit", e))?;
        Ok(id.to_string())
    }

    /// Write an annotated tag object and return its id.
    #[napi]
    pub fn write_tag(
        &self,
        name: String,
        target: String,
        target_kind: String,
        message: String,
    ) -> Result<String> {
        let r = thread_local(&self.inner);
        let target_oid = parse_oid(&target)?;
        let kind = match target_kind.to_lowercase().as_str() {
            "commit" => gix::object::Kind::Commit,
            "tree" => gix::object::Kind::Tree,
            "blob" => gix::object::Kind::Blob,
            "tag" => gix::object::Kind::Tag,
            _ => return Err(napi::Error::from_reason(format!("invalid object kind: {}", target_kind))),
        };
        let tagger = match r.committer() {
            Some(Ok(s)) => s.to_owned().map_err(|e| err("committer.to_owned", e))?,
            _ => return Err(napi::Error::from_reason("no tagger configured")),
        };
        let tag_obj = gix::objs::Tag {
            target: target_oid,
            target_kind: kind,
            name: name.into(),
            tagger: Some(tagger),
            message: message.into(),
            signature: None,
        };
        let id = r.write_object(&tag_obj).map_err(|e| err("write_tag", e))?;
        Ok(id.to_string())
    }

    /// Write raw object bytes with a specified kind (`commit`, `tree`, `blob`, `tag`).
    #[napi]
    pub fn write_object(&self, kind: String, data: napi::bindgen_prelude::Buffer) -> Result<String> {
        let r = thread_local(&self.inner);
        let k = match kind.to_lowercase().as_str() {
            "commit" => gix::object::Kind::Commit,
            "tree" => gix::object::Kind::Tree,
            "blob" => gix::object::Kind::Blob,
            "tag" => gix::object::Kind::Tag,
            _ => return Err(napi::Error::from_reason(format!("invalid object kind: {}", kind))),
        };
        let id = r
            .write_buf(k, &data)
            .map_err(|e| err("write_object", e))?;
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

    /// Build an (anonymous) remote from `url` without persisting it to config.
    #[napi]
    pub fn remote_at(&self, url: String) -> Result<Remote> {
        let r = thread_local(&self.inner);
        let remote = r
            .remote_at(url.as_str())
            .map_err(|e| err("remote_at", e))?;
        Ok(build_remote(&self.inner, remote))
    }

    // ---- config / paths --------------------------------------------------

    /// All config entries visible to this repository (repo + global + system), in file order.
    /// Each section's values are prefixed with `section[.subsection].`, e.g. `remote.origin.url`.
    #[napi]
    pub fn config_entries(&self) -> Result<Vec<ConfigEntry>> {
        let r = thread_local(&self.inner);
        let snap = r.config_snapshot();
        let mut out = Vec::new();
        for section in snap.sections() {
            let name = lossy(section.header().name());
            let prefix = match section.header().subsection_name() {
                Some(sub) => format!("{}.{}", name, lossy(sub)),
                None => name,
            };
            for key in section.value_names() {
                if let Some(v) = section.value(key.as_str()) {
                    out.push(ConfigEntry {
                        key: format!("{}.{}", prefix, key),
                        value: lossy(&v),
                    });
                }
            }
        }
        Ok(out)
    }

    /// Read a single config value as a string.
    #[napi]
    pub fn config_string(&self, key: String) -> Option<String> {
        let r = thread_local(&self.inner);
        r.config_snapshot().string(key.as_str()).map(|v| lossy(&*v))
    }

    // ---- index (staging area) ----------------------------------------------

    /// All entries currently in the index (staging area).
    #[napi]
    pub fn index(&self) -> Result<Vec<IndexEntry>> {
        let r = thread_local(&self.inner);
        let file = r.index().map_err(|e| err("index", e))?;
        let mut out = Vec::with_capacity(file.entries().len());
        for entry in file.entries() {
            let stage = match entry.flags.stage() {
                gix::index::entry::Stage::Unconflicted => 0,
                gix::index::entry::Stage::Base => 1,
                gix::index::entry::Stage::Ours => 2,
                gix::index::entry::Stage::Theirs => 3,
            };
            out.push(IndexEntry {
                path: lossy(entry.path(&file)),
                mode: format!("{:?}", entry.mode),
                id: entry.id.to_string(),
                stage,
            });
        }
        Ok(out)
    }

    /// Compute the working-tree + index status (`git status`), returning one entry per change.
    ///
    /// Async: the (blocking) status walk runs on a libuv worker thread and resolves a
    /// `Promise<StatusEntry[]>`. If `on_progress` is provided, progress updates are streamed to it
    /// from the worker thread via a `ThreadsafeFunction`.
    #[napi(ts_return_type = "Promise<StatusEntry[]>")]
    pub fn status(
        &self,
        on_progress: Option<ThreadsafeFunction<ProgressEvent, ()>>,
    ) -> Result<AsyncTask<StatusTask>> {
        Ok(AsyncTask::new(StatusTask {
            inner: self.inner.clone(),
            on_progress: on_progress.map(Arc::new),
        }))
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

    /// Whether this repository is a shallow clone.
    #[napi]
    pub fn is_shallow(&self) -> Result<bool> {
        let r = thread_local(&self.inner);
        r.is_shallow().map_err(|e| err("is_shallow", e))
    }

    /// The shallow boundary commits (empty when not a shallow clone).
    #[napi]
    pub fn shallow_commits(&self) -> Result<Vec<String>> {
        let r = thread_local(&self.inner);
        let commits = r
            .shallow_commits()
            .map_err(|e| err("shallow_commits", e))?;
        Ok(commits
            .map(|c| c.iter().map(|id| id.to_string()).collect())
            .unwrap_or_default())
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

    /// Run `git describe` on `rev`: the closest annotated tag, falling back to an abbreviated id.
    #[napi]
    pub fn describe(&self, rev: String) -> Result<String> {
        let r = thread_local(&self.inner);
        let id = r.rev_parse_single(rev.as_str()).map_err(|e| err("describe", e))?;
        let commit = r
            .find_commit(id)
            .map_err(|e| err("describe", e))?;
        let mut platform = commit.describe();
        let fmt = platform.format().map_err(|e| err("describe", e))?;
        Ok(format!("{}", fmt))
    }

    /// Whether `ancestor` is an ancestor of (or equal to) `descendant`.
    #[napi]
    pub fn is_ancestor(&self, ancestor: String, descendant: String) -> Result<bool> {
        let r = thread_local(&self.inner);
        let a = parse_oid(&ancestor)?;
        let base = r
            .merge_base(a, parse_oid(&descendant)?)
            .map_err(|e| err("merge_base", e))?;
        Ok(base.map(|id| id.to_string()) == Some(a.to_string()))
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
