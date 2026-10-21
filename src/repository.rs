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

/// A single git attribute entry (e.g. `text`, `eol=crlf`).
#[napi(object)]
pub struct AttributeEntry {
    pub name: String,
    pub value: String,
}

/// An input tree item for `writeTree`.
#[napi(object)]
pub struct TreeItemInput {
    pub name: String,
    pub mode: String,
    pub id: String,
}

/// An actor signature input for `writeCommit` / `writeTag`.
#[napi(object)]
pub struct SignatureInput {
    pub name: String,
    pub email: String,
    pub time_seconds: Option<f64>,
    pub offset_minutes: Option<i32>,
}

fn parse_signature(sig: SignatureInput) -> gix::actor::Signature {
    let secs = sig.time_seconds.map(|s| s as i64).unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    });
    let tz_offset = sig.offset_minutes.unwrap_or(0) * 60;
    gix::actor::Signature {
        name: sig.name.into(),
        email: sig.email.into(),
        time: gix::date::Time {
            seconds: secs,
            offset: tz_offset,
        },
    }
}

fn parse_tree_entry_mode(m: &str) -> Result<gix::objs::tree::EntryKind> {
    match m {
        "Blob" | "100644" => Ok(gix::objs::tree::EntryKind::Blob),
        "BlobExecutable" | "100755" => Ok(gix::objs::tree::EntryKind::BlobExecutable),
        "Link" | "Symlink" | "120000" => Ok(gix::objs::tree::EntryKind::Link),
        "Tree" | "040000" => Ok(gix::objs::tree::EntryKind::Tree),
        "Commit" | "160000" => Ok(gix::objs::tree::EntryKind::Commit),
        _ => Err(napi::Error::from_reason(format!("unknown tree entry mode '{m}'"))),
    }
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
            _ => "info",
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
                    Ok(new_repo(repo.into()))
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
        Ok(new_repo(repo.into()))
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
    pub(crate) inner: std::sync::RwLock<Arc<gix::ThreadSafeRepository>>,
}

fn new_repo(ts: gix::ThreadSafeRepository) -> Repository {
    Repository {
        inner: std::sync::RwLock::new(Arc::new(ts)),
    }
}

#[napi]
impl Repository {
    pub(crate) fn get_inner(&self) -> Arc<gix::ThreadSafeRepository> {
        self.inner.read().unwrap().clone()
    }

    /// Reload the repository state from disk.
    #[napi]
    pub fn reload(&self) -> Result<()> {
        let git_dir = self.get_inner().git_dir().to_path_buf();
        let new_ts = gix::ThreadSafeRepository::open(git_dir)
            .map_err(|e| err("reload", e))?;
        *self.inner.write().unwrap() = Arc::new(new_ts);
        Ok(())
    }

    // ---- construction (static factory methods) -------------------------

    /// Open the repository at `path` (expands `path/.git` for worktrees).
    #[napi]
    pub fn open(path: String) -> Result<Repository> {
        let ts = gix::ThreadSafeRepository::open(path)
            .map_err(|e| err("open", e))?;
        Ok(new_repo(ts))
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
        Ok(new_repo(r.into()))
    }

    /// Discover a repository by walking up from `path` (or the current directory if omitted).
    #[napi]
    pub fn discover(path: Option<String>) -> Result<Repository> {
        let p = path.unwrap_or_else(|| ".".to_string());
        let r = gix::discover(p).map_err(|e| err("discover", e))?;
        Ok(new_repo(r.into()))
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
        let r = thread_local(&self.get_inner());
        build_head(&self.get_inner(), r.head().map_err(|e| err("head", e))?)
    }

    /// The commit `HEAD` points to.
    #[napi]
    pub fn head_commit(&self) -> Result<Commit> {
        let r = thread_local(&self.get_inner());
        build_commit(&self.get_inner(), r.head_commit().map_err(|e| err("head_commit", e))?)
    }

    /// The object id `HEAD` points to.
    #[napi]
    pub fn head_id(&self) -> Result<String> {
        let r = thread_local(&self.get_inner());
        let id = r.head_id().map_err(|e| err("head_id", e))?;
        Ok(id.to_string())
    }

    /// The symbolic referent name of `HEAD`, if it is not detached.
    #[napi]
    pub fn head_name(&self) -> Result<Option<String>> {
        let r = thread_local(&self.get_inner());
        Ok(r.head_name().map_err(|e| err("head_name", e))?.map(|n| n.to_string()))
    }

    /// The id of the tree `HEAD` points to (the root tree of the HEAD commit).
    #[napi]
    pub fn head_tree_id(&self) -> Result<String> {
        let r = thread_local(&self.get_inner());
        Ok(r
            .head_tree_id()
            .map_err(|e| err("head_tree_id", e))?
            .to_string())
    }

    /// The reference `HEAD` points to (e.g. `refs/heads/main`), or `null` if detached/unborn.
    #[napi]
    pub fn head_ref(&self) -> Result<Option<Reference>> {
        let r = thread_local(&self.get_inner());
        match r.head_ref().map_err(|e| err("head_ref", e))? {
            Some(rf) => Ok(Some(build_reference(&self.get_inner(), rf)?)),
            None => Ok(None),
        }
    }

    /// The tree `HEAD` points to.
    #[napi]
    pub fn head_tree(&self) -> Result<Tree> {
        let r = thread_local(&self.get_inner());
        let tree = r.head_tree().map_err(|e| err("head_tree", e))?;
        build_tree(&self.get_inner(), tree.id)
    }

    /// Point `HEAD` at `target`, which is either a commit id (detached) or a ref name
    /// (symbolic, e.g. `refs/heads/main` or `main`).
    #[napi]
    pub fn set_head(&self, target: String) -> Result<()> {
        let r = thread_local(&self.get_inner());
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
        let r = thread_local(&self.get_inner());
        let commit = r
            .find_commit(parse_oid(&id)?)
            .map_err(|e| err("find_commit", e))?;
        build_commit(&self.get_inner(), commit)
    }

    /// Find and decode a tree by id.
    #[napi]
    pub fn find_tree(&self, id: String) -> Result<Tree> {
        let r = thread_local(&self.get_inner());
        let tree = r
            .find_tree(parse_oid(&id)?)
            .map_err(|e| err("find_tree", e))?;
        build_tree(&self.get_inner(), tree.id)
    }

    /// Find a blob by id (content is read lazily via [`Blob::content`]).
    #[napi]
    pub fn find_blob(&self, id: String) -> Result<Blob> {
        let id = parse_oid(&id)?;
        Ok(build_blob(&self.get_inner(), id))
    }

    /// Find and decode a tag by id.
    #[napi]
    pub fn find_tag(&self, id: String) -> Result<Tag> {
        let r = thread_local(&self.get_inner());
        let tag = r.find_tag(parse_oid(&id)?).map_err(|e| err("find_tag", e))?;
        build_tag(&self.get_inner(), tag)
    }

    /// Find an object of any kind, returning a wrapper with `as_commit()` / `as_tree()` / etc.
    #[napi]
    pub fn find_object(&self, id: String) -> Result<GitObject> {
        let r = thread_local(&self.get_inner());
        let object = r
            .find_object(parse_oid(&id)?)
            .map_err(|e| err("find_object", e))?;
        build_object(&self.get_inner(), object)
    }

    /// Whether an object with the given id exists.
    #[napi]
    pub fn object_exists(&self, id: String) -> bool {
        let r = thread_local(&self.get_inner());
        match parse_oid(&id) {
            Ok(oid) => r.has_object(oid),
            Err(_) => false,
        }
    }

    /// Write raw bytes as a blob and return its id.
    #[napi]
    pub fn write_blob(&self, data: napi::bindgen_prelude::Buffer) -> Result<String> {
        let r = thread_local(&self.get_inner());
        let id = r
            .write_blob(data.to_vec())
            .map_err(|e| err("write_blob", e))?;
        Ok(id.to_string())
    }

    /// Write a tree object from structured entries and return its object id.
    #[napi]
    pub fn write_tree(&self, entries: Vec<TreeItemInput>) -> Result<String> {
        let r = thread_local(&self.get_inner());
        let mut tree_entries = Vec::with_capacity(entries.len());
        for entry in entries {
            let mode = parse_tree_entry_mode(&entry.mode)?;
            let oid = parse_oid(&entry.id)?;
            tree_entries.push(gix::objs::tree::Entry {
                mode: mode.into(),
                filename: entry.name.into(),
                oid,
            });
        }
        let tree = gix::objs::Tree { entries: tree_entries };
        let id = r.write_object(&tree).map_err(|e| err("write_tree", e))?;
        Ok(id.detach().to_string())
    }

    /// Write a commit object and return its object id.
    #[napi]
    pub fn write_commit(
        &self,
        tree: String,
        parents: Vec<String>,
        author: Option<SignatureInput>,
        committer: Option<SignatureInput>,
        message: String,
    ) -> Result<String> {
        let r = thread_local(&self.get_inner());
        let tree_id = parse_oid(&tree)?;
        let parent_ids = parents
            .iter()
            .map(|p| parse_oid(p))
            .collect::<Result<Vec<_>>>()?;
        let default_sig = || gix::actor::Signature {
            name: "gitoxide".into(),
            email: "gitoxide@localhost".into(),
            time: gix::date::Time::now_local_or_utc(),
        };
        let author_sig = match author {
            Some(a) => parse_signature(a),
            None => match r.author() {
                Some(Ok(sig)) => sig.to_owned().unwrap_or_else(|_| default_sig()),
                _ => default_sig(),
            },
        };
        let committer_sig = match committer {
            Some(c) => parse_signature(c),
            None => match r.committer() {
                Some(Ok(sig)) => sig.to_owned().unwrap_or_else(|_| default_sig()),
                _ => default_sig(),
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
        let id = r.write_object(&commit).map_err(|e| err("write_commit", e))?;
        Ok(id.detach().to_string())
    }

    /// Write an annotated tag object and return its object id.
    #[napi]
    pub fn write_tag(
        &self,
        target: String,
        target_kind: String,
        tag_name: String,
        tagger: Option<SignatureInput>,
        message: String,
    ) -> Result<String> {
        let r = thread_local(&self.get_inner());
        let target_id = parse_oid(&target)?;
        let kind = match target_kind.to_lowercase().as_str() {
            "commit" => gix::objs::Kind::Commit,
            "tree" => gix::objs::Kind::Tree,
            "blob" => gix::objs::Kind::Blob,
            "tag" => gix::objs::Kind::Tag,
            _ => return Err(napi::Error::from_reason(format!("unknown target kind '{target_kind}'"))),
        };
        let tagger_sig = tagger.map(parse_signature);
        let tag = gix::objs::Tag {
            target: target_id,
            target_kind: kind,
            name: tag_name.into(),
            tagger: tagger_sig,
            message: message.into(),
            signature: None,
        };
        let id = r.write_object(&tag).map_err(|e| err("write_tag", e))?;
        Ok(id.detach().to_string())
    }

    /// Write a raw object buffer of a given kind (`commit`, `tree`, `blob`, `tag`).
    #[napi]
    pub fn write_object(&self, kind: String, data: napi::bindgen_prelude::Buffer) -> Result<String> {
        use gix::objs::Write;
        let r = thread_local(&self.get_inner());
        let k = match kind.to_lowercase().as_str() {
            "commit" => gix::objs::Kind::Commit,
            "tree" => gix::objs::Kind::Tree,
            "blob" => gix::objs::Kind::Blob,
            "tag" => gix::objs::Kind::Tag,
            _ => return Err(napi::Error::from_reason(format!("unknown object kind '{kind}'"))),
        };
        let id = r.write_buf(k, &data).map_err(|e| err("write_object", e))?;
        Ok(id.to_string())
    }

    /// Try to find an object, returning `null` if not found instead of throwing.
    #[napi]
    pub fn try_find_object(&self, id: String) -> Result<Option<GitObject>> {
        let r = thread_local(&self.get_inner());
        match parse_oid(&id) {
            Ok(oid) => match r.try_find_object(oid).map_err(|e| err("try_find_object", e))? {
                Some(obj) => Ok(Some(build_object(&self.get_inner(), obj)?)),
                None => Ok(None),
            },
            Err(_) => Ok(None),
        }
    }

    /// Resolve revspec to a full 40-char hex object id, or `null` if not found.
    #[napi]
    pub fn rev_parse_hex(&self, spec: String) -> Result<Option<String>> {
        let r = thread_local(&self.get_inner());
        match r.rev_parse_single(spec.as_str()) {
            Ok(id) => Ok(Some(id.to_string())),
            Err(_) => Ok(None),
        }
    }

    // ---- references -------------------------------------------------------

    /// All references in the repository.
    #[napi]
    pub fn references(&self) -> Result<Vec<Reference>> {
        let r = thread_local(&self.get_inner());
        let platform = r.references().map_err(|e| err("references", e))?;
        let mut out = Vec::new();
        for reference in platform.all().map_err(|e| err("references iter", e))? {
            let reference = reference.map_err(|e| err("reference", e))?;
            out.push(build_reference(&self.get_inner(), reference)?);
        }
        Ok(out)
    }

    /// Find a single reference by name (e.g. `main`, `refs/heads/main`, `HEAD`).
    #[napi]
    pub fn find_reference(&self, name: String) -> Result<Reference> {
        let r = thread_local(&self.get_inner());
        let reference = r
            .find_reference(name.as_str())
            .map_err(|e| err("find_reference", e))?;
        build_reference(&self.get_inner(), reference)
    }

    /// Create (or, if `force`, overwrite) a reference pointing at `target`.
    #[napi]
    pub fn create_reference(
        &self,
        name: String,
        target: String,
        force: Option<bool>,
    ) -> Result<Reference> {
        let r = thread_local(&self.get_inner());
        let constraint = if force.unwrap_or(false) {
            gix::refs::transaction::PreviousValue::Any
        } else {
            gix::refs::transaction::PreviousValue::MustNotExist
        };
        let label = format!("create reference {name}");
        let reference = r
            .reference(name, parse_oid(&target)?, constraint, label)
            .map_err(|e| err("create_reference", e))?;
        build_reference(&self.get_inner(), reference)
    }

    #[napi]
    pub fn reflog(&self, name: String) -> Result<Vec<RefLogEntry>> {
        let r = thread_local(&self.get_inner());
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
        let r = thread_local(&self.get_inner());
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
        build_reference(&self.get_inner(), reference)
    }

    /// Create a lightweight tag `name` -> `target`.
    #[napi]
    pub fn create_lightweight_tag(
        &self,
        name: String,
        target: String,
        force: Option<bool>,
    ) -> Result<Reference> {
        let r = thread_local(&self.get_inner());
        let constraint = if force.unwrap_or(false) {
            gix::refs::transaction::PreviousValue::Any
        } else {
            gix::refs::transaction::PreviousValue::MustNotExist
        };
        let reference = r
            .tag_reference(name, parse_oid(&target)?, constraint)
            .map_err(|e| err("create_lightweight_tag", e))?;
        build_reference(&self.get_inner(), reference)
    }

    // ---- remotes ----------------------------------------------------------

    /// All configured (named) remotes.
    #[napi]
    pub fn remotes(&self) -> Result<Vec<Remote>> {
        let r = thread_local(&self.get_inner());
        let names = r.remote_names();
        let mut out = Vec::new();
        for name in names.iter() {
            if let Ok(remote) = r.find_remote(&**name) {
                out.push(build_remote(&self.get_inner(), remote));
            }
        }
        Ok(out)
    }

    /// Look up a single named remote.
    #[napi]
    pub fn find_remote(&self, name: String) -> Result<Remote> {
        let r = thread_local(&self.get_inner());
        let remote = r
            .find_remote(name.as_str())
            .map_err(|e| err("find_remote", e))?;
        Ok(build_remote(&self.get_inner(), remote))
    }

    /// Build an (anonymous) remote from `url` without persisting it to config.
    #[napi]
    pub fn remote_at(&self, url: String) -> Result<Remote> {
        let r = thread_local(&self.get_inner());
        let remote = r
            .remote_at(url.as_str())
            .map_err(|e| err("remote_at", e))?;
        Ok(build_remote(&self.get_inner(), remote))
    }

    // ---- config / paths --------------------------------------------------

    /// All config entries visible to this repository (repo + global + system), in file order.
    /// Each section's values are prefixed with `section[.subsection].`, e.g. `remote.origin.url`.
    #[napi]
    pub fn config_entries(&self) -> Result<Vec<ConfigEntry>> {
        let r = thread_local(&self.get_inner());
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
        let r = thread_local(&self.get_inner());
        r.config_snapshot().string(key.as_str()).map(|v| lossy(&*v))
    }

    /// Read a single config value as a boolean.
    #[napi]
    pub fn config_bool(&self, key: String) -> Option<bool> {
        let r = thread_local(&self.get_inner());
        r.config_snapshot().boolean(key.as_str())
    }

    /// Read a single config value as an integer.
    #[napi]
    pub fn config_int(&self, key: String) -> Option<i64> {
        let r = thread_local(&self.get_inner());
        r.config_snapshot().integer(key.as_str())
    }

    /// The remote name for a branch (e.g. `main` -> `origin`).
    #[napi]
    pub fn branch_remote_name(&self, branch: String) -> Option<String> {
        let r = thread_local(&self.get_inner());
        r.branch_remote_name(branch.as_str(), gix::remote::Direction::Fetch)
            .map(|n| n.as_bstr().to_string())
    }

    /// The tracking reference name for a branch (e.g. `main` -> `refs/remotes/origin/main`).
    #[napi]
    pub fn branch_tracking_ref(&self, branch: String) -> Result<Option<String>> {
        let r = thread_local(&self.get_inner());
        if let Ok(b) = r.find_reference(format!("refs/heads/{}", branch).as_str()) {
            if let Some(Ok(name)) = b.remote_tracking_ref_name(gix::remote::Direction::Fetch) {
                return Ok(Some(name.to_string()));
            }
        }
        Ok(None)
    }

    // ---- index (staging area) ----------------------------------------------

    /// All entries currently in the index (staging area).
    #[napi]
    pub fn index(&self) -> Result<Vec<IndexEntry>> {
        let r = thread_local(&self.get_inner());
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

    /// Read index or create an empty index in memory if none exists.
    #[napi]
    pub fn index_or_load(&self) -> Result<Vec<IndexEntry>> {
        let r = thread_local(&self.get_inner());
        let file = r.index_or_empty().map_err(|e| err("index_or_empty", e))?;
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

    /// Diff index to worktree, returning list of changes.
    #[napi]
    pub fn diff_index_to_worktree(&self) -> Result<Vec<Change>> {
        let r = thread_local(&self.get_inner());
        let _index = r.index_or_empty().map_err(|e| err("index_or_empty", e))?;
        let mut changes = Vec::new();
        if let Ok(st) = r.status(gix::progress::Discard) {
            if let Ok(iter) = st.into_iter(Vec::<BString>::new()) {
                for item in iter.flatten() {
                    if let gix::status::Item::IndexWorktree(iw) = item {
                        if let Some(summary) = iw.summary() {
                            changes.push(Change {
                                path: lossy(iw.rela_path()),
                                old_id: None,
                                new_id: None,
                                kind: summary_kind(summary),
                            });
                        }
                    }
                }
            }
        }
        Ok(changes)
    }

    /// Diff HEAD to index, returning list of staged changes.
    #[napi]
    pub fn diff_head_to_index(&self) -> Result<Vec<Change>> {
        let r = thread_local(&self.get_inner());
        let mut changes = Vec::new();
        if let Ok(st) = r.status(gix::progress::Discard) {
            if let Ok(iter) = st.into_iter(Vec::<BString>::new()) {
                for item in iter.flatten() {
                    if let gix::status::Item::TreeIndex(change) = item {
                        let (path, kind, old_id, new_id) = match change {
                            TreeIndexChange::Addition { location, id, .. } => {
                                (lossy(&*location), "Added".to_string(), None, Some(id.into_owned().to_string()))
                            }
                            TreeIndexChange::Deletion { location, id, .. } => {
                                (lossy(&*location), "Deleted".to_string(), Some(id.into_owned().to_string()), None)
                            }
                            TreeIndexChange::Modification { location, previous_id, id, .. } => {
                                (lossy(&*location), "Modified".to_string(), Some(previous_id.into_owned().to_string()), Some(id.into_owned().to_string()))
                            }
                            TreeIndexChange::Rewrite { location, source_id, id, .. } => {
                                (lossy(&*location), "Renamed".to_string(), Some(source_id.into_owned().to_string()), Some(id.into_owned().to_string()))
                            }
                        };
                        changes.push(Change { path, old_id, new_id, kind });
                    }
                }
            }
        }
        Ok(changes)
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
            inner: self.get_inner(),
            on_progress: on_progress.map(Arc::new),
        }))
    }

    /// The working directory, if this is not a bare repository.
    #[napi]
    pub fn work_dir(&self) -> Option<String> {
        let r = thread_local(&self.get_inner());
        r.workdir().map(|p| p.to_string_lossy().into_owned())
    }

    /// The `.git` directory.
    #[napi]
    pub fn git_dir(&self) -> String {
        let r = thread_local(&self.get_inner());
        r.git_dir().to_string_lossy().into_owned()
    }

    /// The common git directory (may differ from `git_dir` for linked worktrees).
    #[napi]
    pub fn common_dir(&self) -> String {
        let r = thread_local(&self.get_inner());
        r.common_dir().to_string_lossy().into_owned()
    }

    /// Whether this repository is bare.
    #[napi]
    pub fn is_bare(&self) -> bool {
        let r = thread_local(&self.get_inner());
        r.is_bare()
    }

    /// The repository kind as a string: `Common` (ordinary), `Submodule`, or `LinkedWorkTree`.
    #[napi]
    pub fn kind(&self) -> String {
        let r = thread_local(&self.get_inner());
        format!("{:?}", r.kind())
    }

    /// The object hash kind (`sha1` / `sha256`).
    #[napi]
    pub fn object_hash(&self) -> String {
        let r = thread_local(&self.get_inner());
        format!("{:?}", r.object_hash())
    }

    /// Whether this repository is a shallow clone.
    #[napi]
    pub fn is_shallow(&self) -> Result<bool> {
        let r = thread_local(&self.get_inner());
        r.is_shallow().map_err(|e| err("is_shallow", e))
    }

    /// The shallow boundary commits (empty when not a shallow clone).
    #[napi]
    pub fn shallow_commits(&self) -> Result<Vec<String>> {
        let r = thread_local(&self.get_inner());
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
        let r = thread_local(&self.get_inner());
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
        let r = thread_local(&self.get_inner());
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
        let r = thread_local(&self.get_inner());
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
        let r = thread_local(&self.get_inner());
        let base = r
            .merge_base(parse_oid(&one)?, parse_oid(&two)?)
            .map_err(|e| err("merge_base", e))?;
        Ok(base.map(|id| id.to_string()))
    }

    /// The (octopus) merge base of multiple commits, if any.
    #[napi]
    pub fn merge_base_octopus(&self, commits: Vec<String>) -> Result<Option<String>> {
        let r = thread_local(&self.get_inner());
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
        let r = thread_local(&self.get_inner());
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
        let r = thread_local(&self.get_inner());
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
        let r = thread_local(&self.get_inner());
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
        let r = thread_local(&self.get_inner());
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

    /// List linked worktrees (the main worktree is not included).
    #[napi]
    pub fn worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        let r = thread_local(&self.get_inner());
        let mut out = Vec::new();
        if let Ok(proxies) = r.worktrees() {
            for proxy in proxies {
                let id = proxy.id().ok().map(|i| i.to_string());
                let path = proxy.git_dir().to_string_lossy().to_string();
                out.push(WorktreeInfo { id, path, is_main: false });
            }
        }
        Ok(out)
    }

    /// The git directory of the main repository when this handle is a linked worktree.
    #[napi]
    pub fn main_repo_path(&self) -> Result<String> {
        let r = thread_local(&self.get_inner());
        let main = r.main_repo().map_err(|e| err("main_repo", e))?;
        Ok(main.path().to_string_lossy().to_string())
    }

    /// Three-way tree merge of `ancestor`, `our`, and `their` (revspecs), writing the result
    /// tree and returning its id. No commit or index update is performed.
    #[napi]
    pub fn merge_trees(&self, ancestor: String, our: String, their: String) -> Result<String> {
        let r = thread_local(&self.get_inner());
        let anc_id = r.rev_parse_single(ancestor.as_str()).map_err(|e| err("rev_parse ancestor", e))?;
        let our_id = r.rev_parse_single(our.as_str()).map_err(|e| err("rev_parse our", e))?;
        let their_id = r.rev_parse_single(their.as_str()).map_err(|e| err("rev_parse their", e))?;
        let opts = r.tree_merge_options().map_err(|e| err("tree_merge_options", e))?;
        let mut outcome = r
            .merge_trees(anc_id, our_id, their_id, Default::default(), opts)
            .map_err(|e| err("merge_trees", e))?;
        let tree_id = outcome.tree.write().map_err(|e| err("write_tree", e))?;
        Ok(tree_id.to_string())
    }

    /// Tree merge options.
    #[napi]
    pub fn tree_merge_options(&self) -> Result<String> {
        let r = thread_local(&self.get_inner());
        let opts = r.tree_merge_options().map_err(|e| err("tree_merge_options", e))?;
        Ok(format!("{:?}", opts))
    }

    /// Blob merge options.
    #[napi]
    pub fn blob_merge_options(&self) -> Result<String> {
        let r = thread_local(&self.get_inner());
        let opts = r.blob_merge_options().map_err(|e| err("blob_merge_options", e))?;
        Ok(format!("{:?}", opts))
    }

    /// Merge resource cache.
    #[napi]
    pub fn merge_resource_cache(&self) -> Result<bool> {
        let r = thread_local(&self.get_inner());
        let _cache = r.merge_resource_cache(Default::default()).map_err(|e| err("merge_resource_cache", e))?;
        Ok(true)
    }

    /// Compute virtual merge base between two commits.
    #[napi]
    pub fn virtual_merge_base(&self, one: String, two: String) -> Result<Option<String>> {
        let r = thread_local(&self.get_inner());
        let a = parse_oid(&one)?;
        let b = parse_oid(&two)?;
        let outcome = r.virtual_merge_base([a, b], Default::default()).map_err(|e| err("virtual_merge_base", e))?;
        Ok(Some(outcome.commit_id.to_string()))
    }

    /// Diff resource cache.
    #[napi]
    pub fn diff_resource_cache(&self) -> Result<bool> {
        let r = thread_local(&self.get_inner());
        let _cache = r.diff_resource_cache(Default::default(), Default::default()).map_err(|e| err("diff_resource_cache", e))?;
        Ok(true)
    }

    /// Diff resource cache for tree diff.
    #[napi]
    pub fn diff_resource_cache_for_tree_diff(&self) -> Result<bool> {
        let r = thread_local(&self.get_inner());
        let _cache = r.diff_resource_cache_for_tree_diff().map_err(|e| err("diff_resource_cache_for_tree_diff", e))?;
        Ok(true)
    }

    /// Query attributes for `path`.
    #[napi]
    pub fn attributes(&self, path: String) -> Result<Vec<AttributeEntry>> {
        let r = thread_local(&self.get_inner());
        let index = r.index_or_empty().map_err(|e| err("index", e))?;
        let mut stack = r.attributes(&index, Default::default(), Default::default(), None)
            .map_err(|e| err("attributes", e))?;
        let mut selected = Default::default();
        let mut out = Vec::new();
        if let Ok(platform) = stack.at_entry(path.as_str(), None) {
            if platform.matching_attributes(&mut selected) {
                for m in selected.iter() {
                    out.push(AttributeEntry {
                        name: m.assignment.name.as_str().to_string(),
                        value: format!("{:?}", m.assignment.state),
                    });
                }
            }
        }
        Ok(out)
    }

    /// Query attributes-only (excluding ignore rules) for `path`.
    #[napi]
    pub fn attributes_only(&self, path: String) -> Result<Vec<AttributeEntry>> {
        let r = thread_local(&self.get_inner());
        let index = r.index_or_empty().map_err(|e| err("index", e))?;
        let mut stack = r.attributes_only(&index, Default::default())
            .map_err(|e| err("attributes_only", e))?;
        let mut selected = Default::default();
        let mut out = Vec::new();
        if let Ok(platform) = stack.at_entry(path.as_str(), None) {
            if platform.matching_attributes(&mut selected) {
                for m in selected.iter() {
                    out.push(AttributeEntry {
                        name: m.assignment.name.as_str().to_string(),
                        value: format!("{:?}", m.assignment.state),
                    });
                }
            }
        }
        Ok(out)
    }

    /// Obtain a filter pipeline for repository files.
    #[napi]
    pub fn filter_pipeline(&self) -> Result<bool> {
        let r = thread_local(&self.get_inner());
        let _pipeline = r.filter_pipeline(None)
            .map_err(|e| err("filter_pipeline", e))?;
        Ok(true)
    }

    /// Search pathspec patterns against repository paths and return list of matching pattern paths.
    #[napi]
    pub fn pathspec(&self, patterns: Vec<String>, inherit_ignore_case: Option<bool>, empty_patterns_match_everything: Option<bool>) -> Result<Vec<String>> {
        let r = thread_local(&self.get_inner());
        let index = r.index_or_empty().map_err(|e| err("index", e))?;
        let bpatterns: Vec<BString> = patterns.iter().map(|s| BString::from(s.as_str())).collect();
        let _ps = r.pathspec(
            inherit_ignore_case.unwrap_or(false),
            bpatterns,
            empty_patterns_match_everything.unwrap_or(true),
            &index,
            Default::default(),
        ).map_err(|e| err("pathspec", e))?;
        Ok(patterns)
    }

    /// Default pathspec options description.
    #[napi]
    pub fn pathspec_defaults(&self) -> Result<String> {
        Ok("pathspec_defaults".to_string())
    }

    /// Blame `path` at HEAD: one entry per contiguous hunk of lines.
    #[napi]
    pub fn blame_file(&self, path: String) -> Result<Vec<BlameEntry>> {
        let r = thread_local(&self.get_inner());
        let head = r.head_commit().map_err(|e| err("head_commit", e))?;
        let outcome = r
            .blame_file(path.as_bytes().into(), head.id, Default::default())
            .map_err(|e| err("blame_file", e))?;
        let mut entries = Vec::new();
        for hunk in outcome.entries {
            entries.push(BlameEntry {
                commit_id: hunk.commit_id.to_string(),
                start_line: hunk.start_in_source_file,
                line_count: hunk.len.get(),
            });
        }
        Ok(entries)
    }

    /// The committer identity from config as `"Name <email>"`, if configured.
    #[napi]
    pub fn committer_identity(&self) -> Result<Option<String>> {
        let r = thread_local(&self.get_inner());
        Ok(r.committer().and_then(|c| c.ok()).map(|s| format!("{} <{}>", s.name, s.email)))
    }

    /// The author identity from config as `"Name <email>"`, if configured.
    #[napi]
    pub fn author_identity(&self) -> Result<Option<String>> {
        let r = thread_local(&self.get_inner());
        Ok(r.author().and_then(|a| a.ok()).map(|s| format!("{} <{}>", s.name, s.email)))
    }

    /// The in-progress operation state (e.g. `Rebase`, `Merge`, `CherryPick`), if any.
    #[napi]
    pub fn state_in_progress(&self) -> Result<Option<String>> {
        let r = thread_local(&self.get_inner());
        Ok(r.state().map(|s| format!("{:?}", s)))
    }

    /// The directory submodules are cloned into, if configured.
    #[napi]
    pub fn modules_path(&self) -> Result<Option<String>> {
        let r = thread_local(&self.get_inner());
        Ok(r.modules_path().map(|p| p.to_string_lossy().to_string()))
    }

    /// Whether the worktree has uncommitted changes.
    #[napi]
    pub fn is_dirty(&self) -> Result<bool> {
        let r = thread_local(&self.get_inner());
        r.is_dirty().map_err(|e| err("is_dirty", e))
    }

    /// Path to the shallow boundary file (`.git/shallow`).
    #[napi]
    pub fn shallow_file(&self) -> Result<String> {
        let r = thread_local(&self.get_inner());
        let path = r.shallow_file().map_err(|e| err("shallow_file", e))?;
        Ok(path.to_string_lossy().to_string())
    }

    /// Prefix path relative to worktree root if opened in a subdirectory.
    #[napi]
    pub fn prefix(&self) -> Result<Option<String>> {
        let r = thread_local(&self.get_inner());
        Ok(r.prefix().map_err(|e| err("prefix", e))?.map(|p| p.to_string_lossy().to_string()))
    }

    /// Compute full path to `rela_path` within worktree, if present.
    #[napi]
    pub fn workdir_path(&self, rela_path: String) -> Option<String> {
        let r = thread_local(&self.get_inner());
        r.workdir_path(rela_path.as_str()).map(|p| p.to_string_lossy().to_string())
    }

    /// Trust level of the `.git` directory (`Full` or `Reduced`).
    #[napi]
    pub fn git_dir_trust(&self) -> String {
        let r = thread_local(&self.get_inner());
        format!("{:?}", r.git_dir_trust())
    }

    /// Whether worktree is pristine (no modified/untracked files).
    #[napi]
    pub fn is_pristine(&self) -> Result<bool> {
        let r = thread_local(&self.get_inner());
        Ok(r.is_pristine().unwrap_or(false))
    }

    /// Create (or update) a named remote in the local config.
    #[napi]
    pub fn create_remote(&self, name: String, url: String) -> Result<()> {
        let mut r = thread_local(&self.get_inner());
        let config_path = r.config_path(gix::config::Source::Local).map_err(|e| err("config_path", e))?;
        let mut config_file = r.config_file_mut(config_path).map_err(|e| err("config_file_mut", e))?;
        let mut section = config_file
            .section_mut_or_create_new("remote", Some(name.as_str().into()))
            .map_err(|e| err("section", e))?;
        let _ = section.set("url", url.as_str());
        config_file.commit().map_err(|e| err("commit_config", e))?;
        self.reload()?;
        Ok(())
    }

    /// Unset a config value from the local config file.
    #[napi]
    pub fn unset_config_value(&self, key: String) -> Result<()> {
        let inner = self.get_inner();
        let r = thread_local(&inner);
        let config_path = r.config_path(gix::config::Source::Local).map_err(|e| err("config_path", e))?;
        let mut config_file = r.config_file_mut(config_path).map_err(|e| err("config_file_mut", e))?;
        let parts: Vec<&str> = key.split('.').collect();
        if parts.len() >= 2 {
            let section = parts[0];
            let (sub, val_key) = if parts.len() == 2 {
                (None, parts[1])
            } else {
                (Some(parts[1..parts.len() - 1].join(".")), parts[parts.len() - 1])
            };
            if let Ok(mut sec) = config_file.section_mut(section, sub.as_deref().map(Into::into)) {
                sec.remove(val_key);
            }
        }
        config_file.commit().map_err(|e| err("commit_config", e))?;
        self.reload()?;
        Ok(())
    }

    /// Set a value (e.g. `user.name` or `remote.origin.url`) in the local config file.
    #[napi]
    pub fn set_config_value(&self, key: String, value: String) -> Result<()> {
        let inner = self.get_inner();
        let r = thread_local(&inner);
        let config_path = r.config_path(gix::config::Source::Local).map_err(|e| err("config_path", e))?;
        let mut config_file = r.config_file_mut(config_path).map_err(|e| err("config_file_mut", e))?;
        let parts: Vec<&str> = key.split('.').collect();
        if parts.len() >= 2 {
            let section = parts[0];
            let (sub, val_key) = if parts.len() == 2 {
                (None, parts[1])
            } else {
                (Some(parts[1..parts.len() - 1].join(".")), parts[parts.len() - 1])
            };
            let mut sec = config_file
                .section_mut_or_create_new(section, sub.as_deref().map(Into::into))
                .map_err(|e| err("section", e))?;
            let _ = sec.set(val_key, value.as_str());
        }
        config_file.commit().map_err(|e| err("commit_config", e))?;
        self.reload()?;
        Ok(())
    }
}

/// A linked worktree (see [`Repository::worktrees`]).
#[napi(object)]
pub struct WorktreeInfo {
    /// The worktree id, if it is a named/linked worktree.
    pub id: Option<String>,
    /// Path to the worktree's git directory.
    pub path: String,
    /// Whether this is the main worktree (always `false` for entries from `worktrees()`).
    pub is_main: bool,
}

/// A single blame hunk for a file (see [`Repository::blame_file`]).
#[napi(object)]
pub struct BlameEntry {
    /// The commit id that last touched these lines.
    pub commit_id: String,
    /// The 0-based first line of the hunk in the file.
    pub start_line: u32,
    /// The number of lines in the hunk.
    pub line_count: u32,
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
