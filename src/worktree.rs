use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use napi::Task;
use napi::bindgen_prelude::AsyncTask;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use crate::commit::Commit;
use crate::error::{err, err_str, parse_oid, thread_local};
use crate::repository::Repository;

/// A secondary worktree descriptor.
#[derive(Clone)]
#[napi(object)]
pub struct WorktreeProxy {
    pub id: String,
    pub git_dir: String,
    /// The absolute checkout location, resolved from the registration's `gitdir`
    /// file, or `null` if it cannot be read. The location might not exist.
    pub base: Option<String>,
    /// Whether the worktree cannot be pruned, moved or deleted.
    pub is_locked: bool,
    /// Whether the worktree can be pruned without an expiry grace period.
    pub is_prunable: bool,
    /// The reason for the lock, if the worktree is locked.
    pub lock_reason: Option<String>,
}

/// Map a gix worktree proxy to its NAPI object. Used as a macro because
/// [`Proxy`](gix::worktree::proxy::Proxy) is a private type that cannot be named.
macro_rules! proxy_to_napi {
    ($p:expr) => {{
        let p = &$p;
        WorktreeProxy {
            id: String::from_utf8_lossy(p.id().map_err(|e| err("worktree_id", e))?.as_ref())
                .into_owned(),
            git_dir: p.git_dir().to_string_lossy().into_owned(),
            base: p.base().ok().map(|b| b.to_string_lossy().into_owned()),
            is_locked: p.is_locked(),
            is_prunable: p.is_prunable(),
            lock_reason: p
                .lock_reason()
                .map(|r| String::from_utf8_lossy(&r).into_owned()),
        }
    }};
}

/// The worktree this repository instance is positioned in, if any.
#[napi(object)]
pub struct WorktreeInfo {
    /// The base of the work tree (checkout root).
    pub base: String,
    /// Whether this is the main worktree of a non-bare repository.
    pub is_main: bool,
    /// Whether the `.git` file or directory exists within the worktree.
    pub dot_git_exists: bool,
}

/// How to treat an existing local branch when adding a worktree.
#[derive(Clone, Copy)]
#[napi]
pub enum WorktreeHeadKind {
    /// Check out an existing local branch.
    Attached,
    /// Check out a commit with a detached `HEAD`.
    Detached,
}

/// The amount of validation to bypass when removing a linked worktree.
#[derive(Clone, Copy)]
#[napi]
pub enum WorktreeRemoveForce {
    /// Refuse to remove a dirty worktree or one containing initialized submodules.
    Never,
    /// Discard changes and initialized submodules, but retain locked worktrees.
    DiscardChanges,
    /// Also override a worktree lock.
    OverrideLock,
}

fn to_remove_force(force: Option<WorktreeRemoveForce>) -> gix::worktree::remove::Force {
    match force {
        Some(WorktreeRemoveForce::DiscardChanges) => gix::worktree::remove::Force::DiscardChanges,
        Some(WorktreeRemoveForce::OverrideLock) => gix::worktree::remove::Force::OverrideLock,
        _ => gix::worktree::remove::Force::Never,
    }
}

/// Options controlling how a worktree is pruned, mirroring git2's
/// `WorktreePruneOptions`. All fields default to `false`.
#[napi(object)]
pub struct WorktreePruneOptions {
    /// Also prune worktrees whose working tree still exists on disk. Default `false`.
    pub valid: Option<bool>,
    /// Also prune locked worktrees. Default `false`.
    pub locked: Option<bool>,
    /// Also remove the working tree directory from the filesystem. Default `false`.
    pub working_tree: Option<bool>,
}

/// Resolve prune options to concrete flags, defaulting every unset field to `false`.
fn resolve_prune_opts(opts: &Option<WorktreePruneOptions>) -> (bool, bool, bool) {
    let o = opts.as_ref();
    (
        o.and_then(|o| o.valid).unwrap_or(false),
        o.and_then(|o| o.locked).unwrap_or(false),
        o.and_then(|o| o.working_tree).unwrap_or(false),
    )
}

/// One materialized entry of a worktree stream.
///
/// Deliberately plain data — no `Buffer` and no repository handle — so the type is
/// `Send` and can be produced by [`StreamTask`] on the libuv thread pool. Unlike
/// `EntryOpts`, which carries a `Buffer` and an optional `Repository`, this cannot
/// be used to construct entries; it only describes ones that already exist.
#[napi(object)]
pub struct StreamEntry {
    /// Slash-separated path relative to the repository.
    pub path: String,
    /// Entry mode, Debug-formatted (e.g. `Blob`, `BlobExecutable`, `Link`, `Tree`).
    pub mode: String,
    /// The object id of the entry.
    pub id: String,
    /// The entry size in bytes, or `null` when unknown before reading.
    pub size: Option<f64>,
}

/// Worktree materialisation task running on the libuv thread pool.
pub struct StreamTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    tree_id: String,
}

impl Task for StreamTask {
    type Output = Vec<StreamEntry>;
    type JsValue = Vec<StreamEntry>;

    fn compute(&mut self) -> napi::Result<Vec<StreamEntry>> {
        let r = thread_local(&self.repo);
        let id = parse_oid(&self.tree_id)?;
        let (mut stream, _index) = r
            .worktree_stream(id)
            .map_err(|e| err("worktree_stream", e))?;
        let mut out = Vec::new();
        while let Some(mut entry) = stream.next_entry().map_err(|e| err("stream_entry", e))? {
            // Capture the metadata first, then drain the entry: gix panics on the
            // next `next_entry()` call if an entry is dropped before exhaustion.
            let path = String::from_utf8_lossy(entry.relative_path().as_ref()).into_owned();
            let mode = format!("{:?}", entry.mode);
            let id = entry.id.to_string();
            let size = entry.bytes_remaining().map(|n| n as f64);
            std::io::copy(&mut entry, &mut std::io::sink())
                .map_err(|e| err("stream_entry_read", e))?;
            out.push(StreamEntry {
                path,
                mode,
                id,
                size,
            });
        }
        Ok(out)
    }

    fn resolve(
        &mut self,
        _env: napi::Env,
        output: Vec<StreamEntry>,
    ) -> napi::Result<Vec<StreamEntry>> {
        Ok(output)
    }
}

/// Worktree-stream raw-bytes task running on the libuv thread pool.
pub struct StreamRawTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    tree_id: String,
}

impl Task for StreamRawTask {
    type Output = Vec<u8>;
    type JsValue = Buffer;

    fn compute(&mut self) -> napi::Result<Vec<u8>> {
        let r = thread_local(&self.repo);
        let id = parse_oid(&self.tree_id)?;
        let (stream, _index) = r
            .worktree_stream(id)
            .map_err(|e| err("worktree_stream", e))?;
        let mut read = stream.into_read();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut read, &mut bytes).map_err(|e| err("stream_encode", e))?;
        Ok(bytes)
    }

    fn resolve(&mut self, _env: napi::Env, output: Vec<u8>) -> napi::Result<Buffer> {
        Ok(Buffer::from(output))
    }
}

/// Worktree addition task running on the libuv thread pool.
pub struct AddWorktreeTask {
    repo: Arc<gix::ThreadSafeRepository>,
    destination: String,
    head_kind: WorktreeHeadKind,
    head_name: String,
    lock: bool,
}

impl AddWorktreeTask {
    pub fn new(
        repo: Arc<gix::ThreadSafeRepository>,
        destination: String,
        head_kind: WorktreeHeadKind,
        head_name: String,
        lock: bool,
    ) -> Self {
        Self {
            repo,
            destination,
            head_kind,
            head_name,
            lock,
        }
    }
}

impl Task for AddWorktreeTask {
    type Output = Worktree;
    type JsValue = Worktree;

    fn compute(&mut self) -> napi::Result<Worktree> {
        let r = thread_local(&self.repo);
        let head = match self.head_kind {
            WorktreeHeadKind::Detached => {
                gix::worktree::add::Head::Detached(parse_oid(&self.head_name)?)
            }
            WorktreeHeadKind::Attached => {
                let full = if self.head_name.starts_with("refs/") {
                    self.head_name.clone()
                } else {
                    format!("refs/heads/{}", self.head_name)
                };
                let name =
                    gix::refs::FullName::try_from(full.as_str()).map_err(|e| err("ref_name", e))?;
                gix::worktree::add::Head::Attached(name)
            }
        };
        let (added, _outcome) = r
            .add_worktree(
                self.destination.as_str(),
                head,
                gix::progress::Discard,
                &AtomicBool::new(false),
            )
            .map_err(|e| err("add_worktree", e))?;
        // The administrative directory of a linked worktree is its real git
        // directory; its file name is the worktree id.
        let git_dir = added.git_dir().to_path_buf();
        let id = git_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| err_str("add_worktree", "could not determine the worktree id"))?;
        let wt = Worktree {
            repo: Arc::clone(&self.repo),
            path: self.destination.clone(),
            id,
            git_dir: git_dir.to_string_lossy().into_owned(),
        };
        if self.lock {
            wt.lock(None)?;
        }
        Ok(wt)
    }

    fn resolve(&mut self, _env: napi::Env, output: Worktree) -> napi::Result<Worktree> {
        Ok(output)
    }
}

/// Worktree removal task running on the libuv thread pool.
pub struct RemoveWorktreeTask {
    repo: Arc<gix::ThreadSafeRepository>,
    target: String,
    force: gix::worktree::remove::Force,
}

impl RemoveWorktreeTask {
    pub fn new(
        repo: Arc<gix::ThreadSafeRepository>,
        target: String,
        force: gix::worktree::remove::Force,
    ) -> Self {
        Self {
            repo,
            target,
            force,
        }
    }
}

impl Task for RemoveWorktreeTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        r.remove_worktree(
            std::path::Path::new(&self.target),
            self.force,
            gix::progress::Discard,
        )
        .map_err(|e| err("remove_worktree", e))
    }

    fn resolve(&mut self, _env: napi::Env, output: ()) -> napi::Result<()> {
        Ok(output)
    }
}

/// Resolve the repository handle implied by `repo`, or by the environment
/// (`GIT_DIR` / `REPO_DIR` / current directory) when it is omitted.
fn repo_or_current(repo: Option<&Repository>) -> napi::Result<Arc<gix::ThreadSafeRepository>> {
    match repo {
        Some(r) => Ok(Arc::clone(&r.inner)),
        None => crate::repository::current_repo(),
    }
}

/// The administrative directory of a worktree: `$GIT_DIR/worktrees/<id>`.
fn admin_dir(repo: &Arc<gix::ThreadSafeRepository>) -> std::path::PathBuf {
    let r = thread_local(repo);
    r.common_dir().join("worktrees")
}

/// A linked working tree attached to a repository.
///
/// A worktree is a checkout directory registered under `$GIT_DIR/worktrees/<id>`,
/// sharing the object database with the repository while keeping its own `HEAD`,
/// index and other per-worktree files.
#[napi]
pub struct Worktree {
    repo: Arc<gix::ThreadSafeRepository>,
    /// The checkout location (working tree root).
    path: String,
    /// The worktree id — the administrative directory name under `$GIT_DIR/worktrees/`.
    id: String,
    /// The absolute administrative directory: `$GIT_DIR/worktrees/<id>`.
    git_dir: String,
}

#[napi]
impl Worktree {
    /// Create a linked worktree at `path`, checked out at `commit` with a detached `HEAD`.
    ///
    /// The repository is taken from `commit`, so no `repo` argument is needed. The
    /// checkout runs on the libuv thread pool.
    #[napi(constructor)]
    pub fn new(path: String, commit: &Commit, lock: Option<bool>) -> napi::Result<Self> {
        Self::add(
            Arc::clone(&commit.repo),
            path,
            gix::worktree::add::Head::Detached(commit.id),
            lock.unwrap_or(false),
        )
    }

    /// Create a linked worktree at `path`, checked out at `commit` with a detached `HEAD`.
    /// Equivalent to `new Worktree(path, commit, lock)`.
    #[napi(factory)]
    pub fn from_commit(path: String, commit: &Commit, lock: Option<bool>) -> napi::Result<Self> {
        Self::new(path, commit, lock)
    }

    /// Open an existing linked worktree by its id (the administrative directory name
    /// under `$GIT_DIR/worktrees/`), like git2's `Repository::find_worktree` plus
    /// `Worktree::open_from_repository`. Returns `null` when no worktree with that id
    /// is registered.
    #[napi]
    pub fn open(id: String, repo: Option<&Repository>) -> napi::Result<Option<Self>> {
        let inner = repo_or_current(repo)?;
        let r = thread_local(&inner);
        let proxy = r
            .worktree_proxy_by_id(id.as_str())
            .map_err(|e| err("worktree_proxy_by_id", e))?;
        match proxy.as_ref() {
            Some(p) => {
                let base = p
                    .base()
                    .map_err(|e| err("worktree.open", e))?
                    .to_string_lossy()
                    .into_owned();
                Ok(Some(Worktree {
                    repo: inner,
                    path: base,
                    id: id.clone(),
                    git_dir: p.git_dir().to_string_lossy().into_owned(),
                }))
            }
            None => Ok(None),
        }
    }

    /// Create a linked worktree at `path` with `HEAD` attached to an existing local
    /// branch (short or full name), which must not be checked out elsewhere.
    ///
    /// `repo` may be omitted, in which case the repository is resolved from the
    /// environment (`GIT_DIR` / `REPO_DIR` / current directory).
    #[napi(factory)]
    pub fn from_branch(
        path: String,
        branch: String,
        repo: Option<&Repository>,
        lock: Option<bool>,
    ) -> napi::Result<Self> {
        let full = if branch.starts_with("refs/") {
            branch
        } else {
            format!("refs/heads/{}", branch)
        };
        let name = gix::refs::FullName::try_from(full.as_str()).map_err(|e| err("ref_name", e))?;
        Self::add(
            repo_or_current(repo)?,
            path,
            gix::worktree::add::Head::Attached(name),
            lock.unwrap_or(false),
        )
    }

    fn add(
        repo: Arc<gix::ThreadSafeRepository>,
        path: String,
        head: gix::worktree::add::Head,
        lock: bool,
    ) -> napi::Result<Self> {
        let r = thread_local(&repo);
        let (added, _outcome) = r
            .add_worktree(
                path.as_str(),
                head,
                gix::progress::Discard,
                &AtomicBool::new(false),
            )
            .map_err(|e| err("add_worktree", e))?;
        // For a linked worktree the opened repository's git dir *is* the
        // administrative directory, whose file name is the worktree id.
        let git_dir = added.git_dir().to_path_buf();
        let id = git_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| err_str("add_worktree", "could not determine the worktree id"))?;
        let wt = Worktree {
            repo,
            path,
            id,
            git_dir: git_dir.to_string_lossy().into_owned(),
        };
        if lock {
            wt.lock(None)?;
        }
        Ok(wt)
    }

    /// The worktree id (the administrative directory name).
    #[napi(getter)]
    pub fn id(&self) -> String {
        self.id.clone()
    }

    /// The checkout location (working tree root).
    #[napi(getter)]
    pub fn path(&self) -> String {
        self.path.clone()
    }

    /// The administrative directory: `$GIT_DIR/worktrees/<id>`.
    #[napi(getter)]
    pub fn git_dir(&self) -> String {
        self.git_dir.clone()
    }

    /// Open the linked worktree as its own repository, sharing the object
    /// database with the main repository. The administrative directory is a
    /// valid git directory, so it is opened directly.
    #[napi]
    pub fn repository(&self) -> napi::Result<Repository> {
        let ts = gix::ThreadSafeRepository::open(self.git_dir.clone())
            .map_err(|e| err("worktree_repository", e))?;
        Ok(Repository {
            inner: Arc::new(ts),
        })
    }

    /// Whether this is the main worktree (rather than a linked one). A linked
    /// worktree's administrative directory is `$GIT_DIR/worktrees/<id>`, whereas
    /// the main worktree's is the repository's common directory.
    #[napi(getter)]
    pub fn is_main(&self) -> bool {
        let r = thread_local(&self.repo);
        let common = r.common_dir().to_string_lossy().into_owned();
        self.git_dir == common
    }

    /// Whether this worktree is locked, which prevents pruning, moving and deletion.
    #[napi]
    pub fn is_locked(&self) -> bool {
        self.locked_path().exists()
    }

    /// The recorded lock reason, or `null` when the worktree is not locked.
    #[napi]
    pub fn lock_reason(&self) -> napi::Result<Option<String>> {
        match std::fs::read_to_string(self.locked_path()) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(err("worktree.lock_reason", e)),
        }
    }

    /// Validate that this worktree still exists on disk and its metadata is consistent,
    /// like git2's `Worktree::validate`. Throws when the working tree, administrative
    /// directory, or the mutual `.git` ↔ `gitdir` links are missing.
    #[napi]
    pub fn validate(&self) -> napi::Result<()> {
        let base = std::path::Path::new(&self.path);
        if !base.exists() {
            return Err(err_str("worktree.validate", "working tree does not exist"));
        }
        let admin = std::path::Path::new(&self.git_dir);
        if !admin.is_dir() {
            return Err(err_str(
                "worktree.validate",
                "administrative directory does not exist",
            ));
        }
        if !base.join(".git").exists() {
            return Err(err_str(
                "worktree.validate",
                "worktree .git link is missing",
            ));
        }
        if !admin.join("gitdir").exists() {
            return Err(err_str(
                "worktree.validate",
                "administrative gitdir file is missing",
            ));
        }
        Ok(())
    }

    /// Whether this worktree can be pruned, like git2's `Worktree::is_prunable`.
    ///
    /// With default options (all flags `false`), only worktrees whose working tree is
    /// missing and that are not locked are prunable. `options.valid` additionally allows
    /// valid (still-present) worktrees; `options.locked` additionally allows locked ones.
    #[napi]
    pub fn is_prunable(&self, options: Option<WorktreePruneOptions>) -> bool {
        let (valid, locked, _working_tree) = resolve_prune_opts(&options);
        let working_exists = std::path::Path::new(&self.path).exists();
        if !valid && working_exists {
            return false;
        }
        if !locked && self.is_locked() {
            return false;
        }
        true
    }

    /// List details of every worktree: the main worktree first, then each linked one.
    ///
    /// `repo` may be omitted, in which case the repository is resolved from the
    /// environment (`GIT_DIR` / `REPO_DIR` / current directory).
    #[napi]
    pub fn list(repo: Option<&Repository>) -> napi::Result<Vec<WorktreeProxy>> {
        let inner = repo_or_current(repo)?;
        let r = thread_local(&inner);
        let proxies = r.worktrees().map_err(|e| err("worktrees", e))?;
        proxies.iter().map(|p| Ok(proxy_to_napi!(p))).collect()
    }

    /// Lock this worktree to prevent its administrative files from being pruned,
    /// and to prevent it from being moved or deleted. An optional `reason` is
    /// stored alongside the lock.
    ///
    /// Implemented directly against `$GIT_DIR/worktrees/<id>/locked` — `gix`
    /// exposes no lock API.
    #[napi]
    pub fn lock(&self, reason: Option<String>) -> napi::Result<()> {
        std::fs::write(self.locked_path(), reason.unwrap_or_default())
            .map_err(|e| err("worktree.lock", e))
    }

    /// Unlock this worktree, allowing it to be pruned, moved or deleted.
    /// Does nothing when the worktree is not locked.
    ///
    /// Implemented directly against `$GIT_DIR/worktrees/<id>/locked`.
    #[napi]
    pub fn unlock(&self) -> napi::Result<()> {
        match std::fs::remove_file(self.locked_path()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(err("worktree.unlock", e)),
        }
    }

    /// Move this worktree to `new_path`, updating both the administrative
    /// `gitdir` file and the worktree's `.git` link. The main worktree and
    /// worktrees containing submodules cannot be moved.
    ///
    /// Implemented directly against the worktree metadata — `gix` exposes no
    /// move API.
    #[napi(js_name = "move")]
    pub fn move_to(&mut self, new_path: String) -> napi::Result<()> {
        let old = std::path::PathBuf::from(&self.path);
        let new = std::path::PathBuf::from(&new_path);
        if old.exists() {
            std::fs::rename(&old, &new).map_err(|e| err("worktree.move", e))?;
        }
        let dot_git = new.join(".git");
        // The worktree's .git file points at the administrative directory …
        std::fs::write(&dot_git, format!("gitdir: {}\n", self.git_dir))
            .map_err(|e| err("worktree.move", e))?;
        // … which in turn points back at the (new) .git file.
        std::fs::write(
            self.gitdir_path(),
            format!("{}\n", dot_git.to_string_lossy()),
        )
        .map_err(|e| err("worktree.move", e))?;
        self.path = new_path;
        Ok(())
    }

    /// Remove this worktree. Only clean worktrees can be removed; `force` permits
    /// discarding changes and submodules, and overriding a lock.
    ///
    /// This is the `gix`-backed removal, run on the libuv thread pool.
    #[napi]
    pub fn remove(
        &self,
        force: Option<WorktreeRemoveForce>,
    ) -> napi::Result<AsyncTask<RemoveWorktreeTask>> {
        Ok(AsyncTask::new(RemoveWorktreeTask::new(
            Arc::clone(&self.repo),
            self.path.clone(),
            to_remove_force(force),
        )))
    }

    /// Remove stale administrative files for worktrees whose working trees are
    /// missing, returning the ids that were pruned. Locked worktrees are skipped
    /// unless `options.locked` is set; valid (still-present) worktrees are skipped
    /// unless `options.valid` is set; `options.working_tree` additionally removes the
    /// working tree directory.
    ///
    /// `repo` may be omitted, in which case the repository is resolved from the
    /// environment. Implemented directly — `gix` exposes no prune API.
    #[napi]
    pub fn prune_all(
        repo: Option<&Repository>,
        opts: Option<WorktreePruneOptions>,
    ) -> napi::Result<Vec<String>> {
        let inner = repo_or_current(repo)?;
        let (valid, locked, working_tree) = resolve_prune_opts(&opts);
        let root = admin_dir(&inner);
        let mut pruned = Vec::new();
        if !root.exists() {
            return Ok(pruned);
        }
        for entry in std::fs::read_dir(&root).map_err(|e| err("worktree.prune", e))? {
            let entry = entry.map_err(|e| err("worktree.prune", e))?;
            let admin = entry.path();
            if !admin.is_dir() {
                continue;
            }
            if !locked && admin.join("locked").exists() {
                continue;
            }
            let target = match std::fs::read_to_string(admin.join("gitdir")) {
                Ok(s) => std::path::PathBuf::from(s.trim().to_string()),
                Err(_) => continue,
            };
            let working_exists = target.exists();
            if !valid && working_exists {
                continue;
            }
            if working_tree && working_exists {
                let _ = std::fs::remove_dir_all(&target);
            }
            std::fs::remove_dir_all(&admin).map_err(|e| err("worktree.prune", e))?;
            if let Some(id) = admin.file_name() {
                pruned.push(id.to_string_lossy().into_owned());
            }
        }
        Ok(pruned)
    }

    /// Remove this worktree's stale administrative files, returning the pruned id
    /// (an empty array when this worktree is not prunable). Honors the same
    /// `options` as the static [`Worktree::pruneAll`]; the main worktree cannot be pruned.
    ///
    /// This is the per-worktree equivalent of git2's `Worktree::prune`.
    #[napi]
    pub fn prune(&self, opts: Option<WorktreePruneOptions>) -> napi::Result<Vec<String>> {
        let (valid, locked, working_tree) = resolve_prune_opts(&opts);
        let admin = std::path::Path::new(&self.git_dir);
        if !admin.is_dir() {
            return Ok(Vec::new());
        }
        if !locked && admin.join("locked").exists() {
            return Ok(Vec::new());
        }
        let target = match std::fs::read_to_string(admin.join("gitdir")) {
            Ok(s) => std::path::PathBuf::from(s.trim().to_string()),
            Err(_) => return Ok(Vec::new()),
        };
        let working_exists = target.exists();
        if !valid && working_exists {
            return Ok(Vec::new());
        }
        if working_tree && working_exists {
            let _ = std::fs::remove_dir_all(&target);
        }
        std::fs::remove_dir_all(admin).map_err(|e| err("worktree.prune", e))?;
        Ok(vec![self.id.clone()])
    }

    /// Repair worktree administrative files that became stale because a worktree
    /// (or the main repository) was moved manually.
    ///
    /// With no `paths`, this worktree's own location is repaired; otherwise each
    /// given path is repaired. Returns the paths that were repaired.
    ///
    /// Implemented directly — `gix` exposes no repair API.
    #[napi]
    pub fn repair(&self, paths: Option<Vec<String>>) -> napi::Result<Vec<String>> {
        let targets = paths.unwrap_or_else(|| vec![self.path.clone()]);
        let mut repaired = Vec::new();
        for p in targets {
            let dir = std::path::PathBuf::from(&p);
            let dot_git = dir.join(".git");
            let content =
                std::fs::read_to_string(&dot_git).map_err(|e| err("worktree.repair", e))?;
            let admin = content
                .trim()
                .strip_prefix("gitdir:")
                .ok_or_else(|| {
                    err_str(
                        "worktree.repair",
                        format!("{} is not a gitdir link", dot_git.display()),
                    )
                })?
                .trim()
                .to_string();
            let admin = std::path::PathBuf::from(admin);
            if !admin.exists() {
                return Err(err_str(
                    "worktree.repair",
                    format!(
                        "administrative directory {} does not exist",
                        admin.display()
                    ),
                ));
            }
            // Re-establish the connection in both directions.
            std::fs::write(
                admin.join("gitdir"),
                format!("{}\n", dot_git.to_string_lossy()),
            )
            .map_err(|e| err("worktree.repair", e))?;
            std::fs::write(&dot_git, format!("gitdir: {}\n", admin.to_string_lossy()))
                .map_err(|e| err("worktree.repair", e))?;
            repaired.push(p);
        }
        Ok(repaired)
    }

    /// Materialize the worktree stream of the tree at `tree_id` into entries
    /// (path, mode, id, size), with filters applied and `export-ignore` honored.
    /// The whole stream is drained on the thread pool.
    #[napi]
    pub fn stream(&self, tree_id: String) -> napi::Result<AsyncTask<StreamTask>> {
        Ok(AsyncTask::new(StreamTask::new(
            Arc::clone(&self.repo),
            tree_id,
        )))
    }

    /// Encode the worktree stream of the tree at `tree_id` into its raw,
    /// unspecified transport bytes.
    #[napi]
    pub fn stream_raw(&self, tree_id: String) -> napi::Result<AsyncTask<StreamRawTask>> {
        Ok(AsyncTask::new(StreamRawTask::new(
            Arc::clone(&self.repo),
            tree_id,
        )))
    }

    fn locked_path(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(&self.git_dir).join("locked")
    }

    fn gitdir_path(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(&self.git_dir).join("gitdir")
    }
}

#[napi]
impl Repository {
    /// List secondary worktrees for this repository.
    #[napi]
    pub fn worktrees(&self) -> napi::Result<Vec<WorktreeProxy>> {
        let r = thread_local(&self.inner);
        let proxies = r.worktrees().map_err(|e| err("worktrees", e))?;
        proxies.iter().map(|p| Ok(proxy_to_napi!(p))).collect()
    }

    /// List all worktrees including the main repository, opening each one.
    /// The main repository comes first, then the linked worktrees in iteration order.
    #[napi]
    pub fn worktrees_including_main(&self) -> napi::Result<Vec<Repository>> {
        let r = thread_local(&self.inner);
        let mut out = Vec::new();
        for item in r
            .worktrees_including_main()
            .map_err(|e| err("worktrees_including_main", e))?
        {
            let repo = item.map_err(|e| err("worktree_repo", e))?;
            out.push(Repository {
                inner: Arc::new(repo.into()),
            });
        }
        Ok(out)
    }

    /// Return the worktree identified by `id` (a directory name under
    /// `.git/worktrees/`), or `null` if it does not exist.
    #[napi]
    pub fn worktree_proxy_by_id(&self, id: String) -> napi::Result<Option<WorktreeProxy>> {
        let r = thread_local(&self.inner);
        let proxy = r
            .worktree_proxy_by_id(id.as_str())
            .map_err(|e| err("worktree_proxy_by_id", e))?;
        Ok(match proxy.as_ref() {
            Some(p) => Some(proxy_to_napi!(p)),
            None => None,
        })
    }

    /// Return the currently set worktree (the validated worktree base path),
    /// or `null` for a bare repository without a registered worktree.
    #[napi]
    pub fn worktree(&self) -> Option<WorktreeInfo> {
        let r = thread_local(&self.inner);
        r.worktree().map(|wt| WorktreeInfo {
            base: wt.base().to_string_lossy().into_owned(),
            is_main: wt.is_main(),
            dot_git_exists: wt.dot_git_exists(),
        })
    }

    /// Whether this repository instance is a linked worktree.
    #[napi]
    pub fn is_worktree(&self) -> bool {
        let r = thread_local(&self.inner);
        r.worktree().map(|wt| !wt.is_main()).unwrap_or(false)
    }

    /// Add and check out a linked worktree at `destination`.
    ///
    /// With `head_kind: "attached"`, `head_name` is an existing local branch
    /// (short or full name) that must not be checked out elsewhere; with
    /// `"detached"`, it is a commit id. Resolves with the opened worktree
    /// repository and the checkout outcome summary.
    #[napi]
    pub fn add_worktree(
        &self,
        destination: String,
        head_kind: WorktreeHeadKind,
        head_name: String,
        lock: Option<bool>,
    ) -> napi::Result<AsyncTask<AddWorktreeTask>> {
        Ok(AsyncTask::new(AddWorktreeTask::new(
            self.inner.clone(),
            destination,
            head_kind,
            head_name,
            lock.unwrap_or(false),
        )))
    }

    /// Resolve `target` (an absolute or relative worktree path, or a unique
    /// suffix of whole path components) and remove the linked worktree.
    ///
    /// `force` controls the safety checks: `never` (default) refuses a dirty
    /// worktree or one containing initialized submodules, `discardChanges`
    /// permits their deletion, `overrideLock` additionally overrides a lock.
    /// The main worktree cannot be removed.
    #[napi]
    pub fn remove_worktree(
        &self,
        target: String,
        force: Option<WorktreeRemoveForce>,
    ) -> napi::Result<AsyncTask<RemoveWorktreeTask>> {
        Ok(AsyncTask::new(RemoveWorktreeTask::new(
            self.inner.clone(),
            target,
            to_remove_force(force),
        )))
    }

    /// Materialize the worktree stream of the tree at `tree_id` into an array of
    /// entries (path, mode, id, size), with filters applied and `export-ignore`
    /// honored. A live stream cannot cross the FFI boundary, so the whole stream
    /// is drained on the thread pool.
    #[napi]
    pub fn worktree_stream(&self, tree_id: String) -> napi::Result<AsyncTask<StreamTask>> {
        Ok(AsyncTask::new(StreamTask::new(self.inner.clone(), tree_id)))
    }

    /// Encode the worktree stream of the tree at `tree_id` into its raw,
    /// unspecified transport bytes — the exact format produced by
    /// `Stream::into_read`, decodable again by `Stream::from_read`.
    ///
    /// This is the low-level counterpart to [`Repository::worktree_stream`]:
    /// instead of draining entries into metadata, it yields the precise bytes
    /// the encoder writes, which can be stored or re-fed into a decoder
    /// elsewhere (e.g. for `git archive`-style transport). Like the
    /// materialized variant, the whole stream is produced on the thread pool.
    #[napi]
    pub fn worktree_stream_raw(&self, tree_id: String) -> napi::Result<AsyncTask<StreamRawTask>> {
        Ok(AsyncTask::new(StreamRawTask::new(
            self.inner.clone(),
            tree_id,
        )))
    }
}
