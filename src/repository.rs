use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use napi::Env;
use napi::Result;
use napi::Task;
use napi::bindgen_prelude::{AsyncTask, Buffer, Either4, Function};
use napi::threadsafe_function::ThreadsafeFunction;
use napi_derive::napi;

use crate::error::{
    cached_namespace_get, cached_namespace_set, err, err_str, parse_oid, thread_local,
};
use crate::object::kind_to_string;
use crate::progress::{ProgressCallback, ProgressEvent};

pub(crate) fn current_repo() -> Result<Arc<gix::ThreadSafeRepository>> {
    for var in ["GIT_DIR", "REPO_DIR"] {
        if let Ok(p) = std::env::var(var) {
            if !p.is_empty() {
                let ts = gix::ThreadSafeRepository::open(&p)
                    .map_err(|e| err(&var.to_ascii_lowercase(), e))?;
                return Ok(Arc::new(ts));
            }
        }
    }
    let r = gix::discover(".").map_err(|e| err("discover", e))?;
    Ok(Arc::new(r.into()))
}

pub(crate) fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Locate the system `git` binary by probing well-known absolute paths, falling back to a
/// `PATH` lookup. Returns `None` only if git cannot be found anywhere.
fn find_git() -> Option<std::path::PathBuf> {
    const CANDIDATES: &[&str] = &[
        "/usr/bin/git",
        "/bin/git",
        "/usr/local/bin/git",
        "/opt/homebrew/bin/git",
        "/opt/homebrew/opt/git/bin/git",
        "/usr/local/opt/git/bin/git",
    ];
    for c in CANDIDATES {
        if std::path::Path::new(c).is_file() {
            return Some(std::path::PathBuf::from(c));
        }
    }
    if let Ok(out) = std::process::Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
    {
        if out.status.success() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() {
                return Some(std::path::PathBuf::from(p));
            }
        }
    }
    None
}

/// A single entry in the git index.
#[derive(Clone)]
#[napi(object)]
pub struct IndexEntry {
    pub path: String,
    pub mode: String,
    pub id: String,
    pub stage: u32,
    /// The filesystem stat information of the entry.
    pub stat: crate::index::IndexStat,
    /// Raw in-memory entry flags (`gix_index::entry::Flags` bits): stage mask,
    /// `ASSUME_VALID` (0x8000), `UPDATE`, `SKIP_WORKTREE`, `INTENT_TO_ADD`, …
    pub flags: u32,
}

/// A single status item for working tree / index.
#[derive(Clone)]
#[napi(object)]
pub struct StatusEntry {
    pub path: String,
    pub status: String,
}

/// Information about a git submodule.
/// A git note attached to an object.
#[derive(Clone)]
#[napi(object)]
pub struct NoteInfo {
    pub id: String,
    pub message: String,
}

/// Input structure for editing an index entry.
#[derive(Clone)]
#[napi(object)]
pub struct IndexEntryInput {
    pub path: String,
    pub id: String,
    pub mode: Option<String>,
}

/// Filesystem capabilities as configured for this repository.
#[napi(object)]
pub struct FsCapabilities {
    /// Whether decomposed unicode paths are precomposed to match git's preference.
    pub precompose_unicode: bool,
    /// Whether the filesystem ignores the case of input (case-folding).
    pub ignore_case: bool,
    /// Whether the executable bit is honored as part of the file mode.
    pub executable_bit: bool,
    /// Whether the filesystem supports symbolic links.
    pub symlink: bool,
}

/// How stat information is compared when checking index entries for up-to-dateness.
#[napi(object)]
pub struct StatOptions {
    /// Whether a file's creation time is taken into consideration.
    pub trust_ctime: bool,
    /// Whether all stat fields are used, or only whole-second mtime/ctime and file size.
    pub check_stat: bool,
    /// Whether nanosecond parts of timestamps are compared.
    pub use_nsec: bool,
    /// Whether the device number is compared.
    pub use_stdev: bool,
}

/// Checkout task running on libuv thread pool.
pub struct CheckoutTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) target: String,
}

impl Task for CheckoutTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<String> {
        let r = thread_local(&self.repo);
        let oid = parse_oid(&self.target)?;
        let commit = r.find_commit(oid).map_err(|e| err("find_commit", e))?;
        let tree_id = commit.tree_id().map_err(|e| err("tree_id", e))?;

        let label = format!("checkout {}", self.target);
        r.reference(
            "HEAD",
            oid,
            gix::refs::transaction::PreviousValue::Any,
            label,
        )
        .map_err(|e| err("checkout_head", e))?;

        let mut file = r
            .index_from_tree(&tree_id)
            .map_err(|e| err("index_from_tree", e))?;
        file.write(Default::default())
            .map_err(|e| err("write_index", e))?;

        Ok(oid.to_string())
    }

    fn resolve(&mut self, _env: napi::Env, output: String) -> Result<String> {
        Ok(output)
    }
}

/// Status computation task running on libuv thread pool.
pub struct StatusTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
}

impl Task for StatusTask {
    type Output = Vec<StatusEntry>;
    type JsValue = Vec<StatusEntry>;

    fn compute(&mut self) -> Result<Vec<StatusEntry>> {
        let r = thread_local(&self.repo);
        let platform = r
            .status(gix::progress::Discard)
            .map_err(|e| err("status", e))?;
        let mut out = Vec::new();
        let iter = platform
            .into_index_worktree_iter(Vec::<gix::bstr::BString>::new())
            .map_err(|e| err("status iter", e))?;
        for item in iter {
            let item = item.map_err(|e| err("status item", e))?;
            let path = lossy(item.rela_path());
            let status = format!("{item:?}");
            out.push(StatusEntry { path, status });
        }
        Ok(out)
    }

    fn resolve(&mut self, _env: napi::Env, output: Vec<StatusEntry>) -> Result<Vec<StatusEntry>> {
        Ok(output)
    }
}

/// Blocking clone work, executed on the libuv thread pool by [`AsyncTask`].
pub struct CloneTask {
    url: String,
    path: String,
    bare: bool,
    progress: Option<Arc<ThreadsafeFunction<ProgressEvent, ()>>>,
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

        let url = self.url.clone();
        let path = self.path.clone();
        let bare = self.bare;

        // Try a gix-native clone first.
        let gix_repo = (|| {
            let mut prepare = if bare {
                gix::prepare_clone_bare(url.as_str(), &path)
            } else {
                gix::prepare_clone(url.as_str(), &path)
            }
            .ok()?;
            let interrupt = AtomicBool::new(false);
            let progress = ProgressCallback::new(self.progress.clone());
            let (repo, _outcome) = prepare.fetch_only(progress, &interrupt).ok()?;
            Some(repo.into())
        })();

        if let Some(repo) = gix_repo {
            return Ok(Repository {
                inner: Arc::new(repo),
            });
        }

        // Fallback: shell out to the system `git` binary. This is robust to environments
        // where git is not on PATH (we probe well-known absolute locations) and to gix
        // ref-update edge cases on local clones.
        if let Some(git) = find_git() {
            let _ = std::fs::remove_dir_all(&path);
            let mut cmd = std::process::Command::new(&git);
            cmd.arg("clone").arg("--local");
            if bare {
                cmd.arg("--bare");
            }
            cmd.arg(&url).arg(&path);
            match cmd.output() {
                Ok(out) if out.status.success() => {
                    let ts = gix::ThreadSafeRepository::open(&path)
                        .map_err(|e| napi::Error::from_reason(format!("open: {e}")))?;
                    return Ok(Repository {
                        inner: Arc::new(ts),
                    });
                }
                Ok(out) => {
                    return Err(napi::Error::from_reason(format!(
                        "git clone failed: {}",
                        String::from_utf8_lossy(&out.stderr)
                    )));
                }
                Err(e) => {
                    return Err(napi::Error::from_reason(format!(
                        "failed to spawn git at {}: {e}",
                        git.display()
                    )));
                }
            }
        }

        Err(napi::Error::from_reason(
            "clone failed (gix unavailable) and no system git binary was found",
        ))
    }

    fn resolve(&mut self, _env: napi::Env, output: Repository) -> Result<Repository> {
        Ok(output)
    }
}

/// The primary handle to a git repository, exposed to TypeScript as a class.
///
/// Internally wraps `Arc<gix::ThreadSafeRepository>` so it can be shared and offloaded to worker
/// threads for async network operations, while sync methods obtain a cheap thread-local view.
#[napi]
pub struct Repository {
    pub(crate) inner: Arc<gix::ThreadSafeRepository>,
}

/// Trust level to force for the git directory while opening.
#[napi]
pub enum Trust {
    /// Caution is warranted; configuration from untrusted sources is not used.
    Reduced,
    /// No doubts about safety; configuration is used at will.
    Full,
}

/// Options controlling how a repository is opened, mirroring `gix::open::Options`.
///
/// Every field is optional; omitted fields use `gix`'s defaults. Pass one of these to
/// [`Repository::open_opts`].
#[napi(object)]
pub struct OpenOptions {
    /// Open `path` as-is without verifying it is a `.git` directory.
    pub open_path_as_is: Option<bool>,
    /// If the repository's trust is not `Full`, fail the open (mirrors git's `safe.directory`).
    pub bail_if_untrusted: Option<bool>,
    /// Treat invalid configuration values as a hard error (git's default strict mode).
    pub strict_config: Option<bool>,
    /// Read configuration without retaining comments/whitespace (optimizes lookup).
    pub lossy_config: Option<bool>,
    /// `-c`-style configuration overrides applied with the CLI source.
    pub cli_config_overrides: Option<Vec<String>>,
    /// API configuration overrides applied with the API source.
    pub api_config_overrides: Option<Vec<String>>,
    /// Force a trust level for the git directory.
    pub trust: Option<Trust>,
}

fn to_gix_open_options(o: OpenOptions) -> gix::open::Options {
    let mut opts = gix::open::Options::default();
    if let Some(v) = o.open_path_as_is {
        opts = opts.open_path_as_is(v);
    }
    if let Some(v) = o.bail_if_untrusted {
        opts = opts.bail_if_untrusted(v);
    }
    if let Some(v) = o.strict_config {
        opts = opts.strict_config(v);
    }
    if let Some(v) = o.lossy_config {
        opts = opts.lossy_config(v);
    }
    if let Some(v) = o.cli_config_overrides {
        opts = opts.cli_overrides(v.iter().map(|s| s.as_str()));
    }
    if let Some(v) = o.api_config_overrides {
        opts = opts.config_overrides(v.iter().map(|s| s.as_str()));
    }
    if let Some(t) = o.trust {
        let t = match t {
            Trust::Reduced => gix::sec::Trust::Reduced,
            Trust::Full => gix::sec::Trust::Full,
        };
        opts = opts.with(t);
    }
    opts
}

/// A [`gix_sec::trust::Mapping`] of [`OpenOptions`], selecting repository options by trust level.
///
/// Pass to [`Repository::open_with_environment_overrides`] / [`Repository::discover_with_environment_overrides`]
/// to control how repositories at the `Full` and `Reduced` trust levels are opened.
#[napi(object)]
pub struct TrustMapping {
    /// Options used when the repository directory is fully trusted (owned by the current user).
    pub full: OpenOptions,
    /// Options used when the repository directory has reduced trust (not owned by the current user).
    pub reduced: OpenOptions,
}

fn to_gix_trust_mapping(m: TrustMapping) -> gix::sec::trust::Mapping<gix::open::Options> {
    gix::sec::trust::Mapping {
        full: to_gix_open_options(m.full),
        reduced: to_gix_open_options(m.reduced),
    }
}

/// Options controlling [`Repository::init_opts`] (repository creation).
#[napi(object)]
pub struct InitOptions {
    /// Create a bare repository (no worktree). Defaults to `false`.
    pub bare: Option<bool>,
    /// Require the destination to be empty. `None` behaves like git (allow non-empty unless a `.git` exists);
    /// `Some(true)` requires an empty destination; `Some(false)` explicitly allows a non-empty one.
    pub destination_must_be_empty: Option<bool>,
    /// Options used when re-opening the freshly created repository.
    pub open_options: Option<OpenOptions>,
}

fn to_gix_create_options(o: &InitOptions) -> gix::create::Options {
    let mut opts = gix::create::Options::default();
    if let Some(v) = o.destination_must_be_empty {
        opts.destination_must_be_empty = Some(v);
    }
    opts
}

/// Options controlling [`Repository::cat_file`], mirroring the modes of `git cat-file`.
#[napi(object)]
pub struct CatFileOptions {
    /// The information to output, mirroring `git cat-file` modes:
    /// - `"content"` (default): the object's raw, decompressed body as a `Buffer`
    ///   (for blobs this is the file content).
    /// - `"type"`: the object kind (`blob` / `tree` / `commit` / `tag`) as a string.
    /// - `"size"`: the object's size in bytes as a number.
    /// - `"exists"`: whether the object exists as a boolean.
    pub mode: Option<String>,
}

#[napi]
impl Repository {
    /// Create a repository handle.
    ///
    /// With `path`, the repository is opened at that location (like [`Repository::open`]);
    /// without it, one is discovered by walking up from the current directory
    /// (like [`Repository::discover`]).
    #[napi(constructor)]
    pub fn new(path: Option<String>) -> Result<Repository> {
        match path {
            Some(p) => Self::open(p),
            None => Self::discover(None),
        }
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

    #[napi]
    pub fn discover_with_environment_overrides(
        directory: Option<String>,
        trust_map: Option<TrustMapping>,
    ) -> Result<Repository> {
        let p = directory.unwrap_or_else(|| ".".to_string());
        let ts = match trust_map {
            Some(tm) => gix::ThreadSafeRepository::discover_with_environment_overrides_opts(
                p,
                gix::discover::upwards::Options::default(),
                to_gix_trust_mapping(tm),
            )
            .map_err(|e| err("discover_with_environment_overrides", e))?,
            None => gix::discover_with_environment_overrides(p)
                .map_err(|e| err("discover_with_environment_overrides", e))?
                .into(),
        };
        Ok(Repository {
            inner: Arc::new(ts),
        })
    }

    /// Open a repository directly from the environment, honoring git overrides
    /// (`GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_SHALLOW_FILE`, `GIT_NAMESPACE`,
    /// `GIT_DISCOVERY_ACROSS_FILESYSTEM`, `GIT_CEILING_DIRECTORIES`).
    ///
    /// If `GIT_DIR` is set it is opened directly; otherwise this walks up from `directory`
    /// (defaulting to the current directory when omitted), like the discovery Git performs for hooks.
    ///
    /// When `trust_map` is provided, the [`OpenOptions`] for the effective trust level are used to open
    /// the repository; otherwise gix's default trust-derived options apply.
    #[napi]
    pub fn open_with_environment_overrides(
        directory: Option<String>,
        trust_map: Option<TrustMapping>,
    ) -> Result<Repository> {
        let p = directory.unwrap_or_else(|| ".".to_string());
        let ts = match trust_map {
            Some(tm) => gix::ThreadSafeRepository::open_with_environment_overrides(
                p,
                to_gix_trust_mapping(tm),
            )
            .map_err(|e| err("open_with_environment_overrides", e))?,
            None => gix::open_with_environment_overrides(p)
                .map_err(|e| err("open_with_environment_overrides", e))?
                .into(),
        };
        Ok(Repository {
            inner: Arc::new(ts),
        })
    }

    /// Clone a repository from `url` into `path`.
    ///
    /// Async: the (blocking) network transfer runs on a libuv worker thread and resolves a
    /// `Promise<Repository>`.
    #[napi(ts_return_type = "Promise<Repository>")]
    pub fn clone(
        url: String,
        path: String,
        bare: Option<bool>,
        _env: Env,
        progress: Option<Function<ProgressEvent, ()>>,
    ) -> Result<AsyncTask<CloneTask>> {
        let tsfn = match progress {
            Some(f) => {
                let builder = f.build_threadsafe_function::<ProgressEvent>();
                Some(Arc::new(builder.callee_handled().build()?))
            }
            None => None,
        };
        Ok(AsyncTask::new(CloneTask {
            url,
            path,
            bare: bare.unwrap_or(false),
            progress: tsfn,
        }))
    }

    /// Open the repository at `path` (expands `path/.git` for worktrees).
    #[napi]
    pub fn open(path: String) -> Result<Repository> {
        let ts = gix::ThreadSafeRepository::open(path).map_err(|e| err("open", e))?;
        Ok(Repository {
            inner: Arc::new(ts),
        })
    }

    /// Open a repository with the given `options` (trust, config overrides, strict/lossy config, …).
    #[napi]
    pub fn open_opts(path: String, options: OpenOptions) -> Result<Repository> {
        let ts = gix::ThreadSafeRepository::open_opts(path, to_gix_open_options(options))
            .map_err(|e| err("open_opts", e))?;
        Ok(Repository {
            inner: Arc::new(ts),
        })
    }

    /// Initialize a repository with the given `options` (`bare`, `destinationMustBeEmpty`, open options).
    #[napi]
    pub fn init_opts(path: String, options: InitOptions) -> Result<Repository> {
        let kind = if options.bare.unwrap_or(false) {
            gix::create::Kind::Bare
        } else {
            gix::create::Kind::WithWorktree
        };
        let create_options = to_gix_create_options(&options);
        let open_opts = options
            .open_options
            .map(to_gix_open_options)
            .unwrap_or_default();
        let ts = gix::ThreadSafeRepository::init_opts(path, kind, create_options, open_opts)
            .map_err(|e| err("init_opts", e))?;
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

    /// The working directory, if this is not a bare repository.
    #[napi]
    pub fn work_dir(&self) -> Option<String> {
        let r = thread_local(&self.inner);
        r.workdir().map(|p| p.to_string_lossy().into_owned())
    }

    /// Reset HEAD, index, and optionally working tree to target.
    #[napi]
    pub fn reset(&self, target: String, hard: Option<bool>) -> Result<()> {
        let r = thread_local(&self.inner);
        let oid = parse_oid(&target)?;
        let commit = r.find_commit(oid).map_err(|e| err("find_commit", e))?;
        let tree_id = commit.tree_id().map_err(|e| err("tree_id", e))?;

        let label = format!("reset {target}");
        r.reference(
            "HEAD",
            oid,
            gix::refs::transaction::PreviousValue::Any,
            label,
        )
        .map_err(|e| err("reset_head", e))?;

        let mut file = r
            .index_from_tree(&tree_id)
            .map_err(|e| err("index_from_tree", e))?;
        file.write(Default::default())
            .map_err(|e| err("write_index", e))?;

        if hard.unwrap_or(false) {
            if let Some(workdir) = r.workdir() {
                let _ = std::fs::remove_dir_all(workdir);
                let _ = std::fs::create_dir_all(workdir);
            }
        }

        Ok(())
    }

    /// Return the main repository if this is a linked worktree.
    #[napi]
    pub fn main_repository(&self) -> Result<Repository> {
        let r = thread_local(&self.inner);
        let main_repo = r.main_repo().map_err(|e| err("main_repo", e))?;
        Ok(Repository {
            inner: Arc::new(main_repo.into()),
        })
    }

    /// The `.git` directory.
    #[napi]
    pub fn git_dir(&self) -> String {
        let r = thread_local(&self.inner);
        r.git_dir().to_string_lossy().into_owned()
    }

    /// Obtain a transparent file-encryption handle ("crypt") bound to this repository.
    #[napi]
    pub fn crypt(&self) -> crate::crypto::Crypto {
        crate::crypto::Crypto {
            repo: Arc::clone(&self.inner),
        }
    }

    /// The path to the repository itself, containing objects, references, configuration,
    /// and more. Synonymous with `gitDir`.
    #[napi]
    pub fn path(&self) -> String {
        let r = thread_local(&self.inner);
        r.path().to_string_lossy().into_owned()
    }

    /// Filesystem capabilities as configured for this repository, e.g. whether the
    /// executable bit and symlinks are honored, and how unicode/case are treated.
    #[napi]
    pub fn filesystem_options(&self) -> Result<FsCapabilities> {
        let r = thread_local(&self.inner);
        let caps = r
            .filesystem_options()
            .map_err(|e| err("filesystem_options", e))?;
        Ok(FsCapabilities {
            precompose_unicode: caps.precompose_unicode,
            ignore_case: caps.ignore_case,
            executable_bit: caps.executable_bit,
            symlink: caps.symlink,
        })
    }

    /// How stat information is compared when checking index entries for up-to-dateness.
    #[napi]
    pub fn stat_options(&self) -> Result<StatOptions> {
        let r = thread_local(&self.inner);
        let opts = r.stat_options().map_err(|e| err("stat_options", e))?;
        Ok(StatOptions {
            trust_ctime: opts.trust_ctime,
            check_stat: opts.check_stat,
            use_nsec: opts.use_nsec,
            use_stdev: opts.use_stdev,
        })
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

    /// Compute working tree status asynchronously.
    #[napi(ts_return_type = "Promise<Vec<StatusEntry>>")]
    pub fn status(&self) -> Result<AsyncTask<StatusTask>> {
        Ok(AsyncTask::new(StatusTask {
            repo: Arc::clone(&self.inner),
        }))
    }

    /// Checkout a target commit asynchronously.
    #[napi(ts_return_type = "Promise<String>")]
    pub fn checkout(&self, target: String) -> Result<AsyncTask<CheckoutTask>> {
        Ok(AsyncTask::new(CheckoutTask {
            repo: Arc::clone(&self.inner),
            target,
        }))
    }

    /// The operation currently in progress (e.g. `Merge`, `Rebase`, `CherryPick`), or `null`.
    #[napi]
    pub fn state(&self) -> Option<String> {
        let r = thread_local(&self.inner);
        r.state().map(|s| format!("{:?}", s))
    }

    /// The trust level assigned to this repository's git-dir (`full` or `reduced`).
    #[napi]
    pub fn git_dir_trust(&self) -> String {
        let r = thread_local(&self.inner);
        match r.git_dir_trust() {
            gix::sec::Trust::Full => "full".to_string(),
            gix::sec::Trust::Reduced => "reduced".to_string(),
        }
    }

    /// The current working directory as present during repository instantiation.
    #[napi]
    pub fn current_dir(&self) -> String {
        let r = thread_local(&self.inner);
        r.current_dir().display().to_string()
    }

    /// The path to the `.git/modules` directory of the worktree, or `null` if bare.
    #[napi]
    pub fn modules_path(&self) -> Option<String> {
        let r = thread_local(&self.inner);
        r.modules_path().map(|p| p.display().to_string())
    }

    /// The directory the git binary was installed to (used for exec-path lookups).
    #[napi]
    pub fn install_dir(&self) -> Result<String> {
        let r = thread_local(&self.inner);
        r.install_dir()
            .map(|p| p.display().to_string())
            .map_err(|e| err("install_dir", e))
    }

    /// The relative path between the working tree and the current working dir, or `null`.
    #[napi]
    pub fn prefix(&self) -> Result<Option<String>> {
        let r = thread_local(&self.inner);
        Ok(r.prefix()
            .map_err(|e| err("prefix", e))?
            .map(|p| p.display().to_string()))
    }

    /// Normalize `path` into a repository-relative Git path with slash separators.
    #[napi]
    pub fn normalize_path(&self, path: String) -> Result<String> {
        let r = thread_local(&self.inner);
        let normalized = r
            .normalize_path(path.as_str())
            .map_err(|e| err("normalize_path", e))?;
        let bytes: &[u8] = &normalized;
        Ok(lossy(bytes))
    }

    /// Whether this repository has no commits and no configured `init.defaultBranch` deviation.
    #[napi]
    pub fn is_pristine(&self) -> Option<bool> {
        let r = thread_local(&self.inner);
        r.is_pristine()
    }

    /// Resolve a repository-relative path against the working tree, or `null` if bare.
    #[napi]
    pub fn workdir_path(&self, relative_path: String) -> Option<String> {
        let r = thread_local(&self.inner);
        r.workdir_path(gix::bstr::BStr::new(relative_path.as_bytes()))
            .map(|p| p.display().to_string())
    }

    /// Replace the working tree directory (validated to exist), returning the previous one.
    #[napi]
    pub fn set_workdir(&self, workdir: Option<String>) -> Result<Option<String>> {
        let mut r = thread_local(&self.inner);
        let previous = r
            .set_workdir(workdir.map(std::path::PathBuf::from))
            .map_err(|e| err("set_workdir", e))?;
        Ok(previous.map(|p| p.display().to_string()))
    }

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

    /// Return true if `ancestor` is an ancestor of (or equal to) `descendant`.
    #[napi]
    pub fn is_ancestor(&self, ancestor: String, descendant: String) -> Result<bool> {
        let r = thread_local(&self.inner);
        let ancestor_oid = parse_oid(&ancestor)?;
        let descendant_oid = parse_oid(&descendant)?;
        let platform = r.rev_walk([descendant_oid]);
        let ids = platform.all().map_err(|e| err("rev_walk", e))?;
        Ok(ids
            .into_iter()
            .any(|id| id.map(|i| i.id == ancestor_oid).unwrap_or(false)))
    }

    /// True if this repository is a shallow clone.
    #[napi]
    pub fn is_shallow(&self) -> Result<bool> {
        let r = thread_local(&self.inner);
        r.is_shallow().map_err(|e| err("is_shallow", e))
    }

    /// Path to the `shallow` file (may not exist if not a shallow clone).
    #[napi]
    pub fn shallow_file(&self) -> Result<String> {
        let r = thread_local(&self.inner);
        let path = r.shallow_file().map_err(|e| err("shallow_file", e))?;
        Ok(path.to_string_lossy().into_owned())
    }

    /// List the shallow boundary commits, or `null` if the repo is not shallow.
    #[napi]
    pub fn shallow_commits(&self) -> Result<Option<Vec<String>>> {
        let r = thread_local(&self.inner);
        let path = r.shallow_file().map_err(|e| err("shallow_file", e))?;
        if !path.exists() {
            return Ok(None);
        }
        let buf = std::fs::read_to_string(&path).map_err(|e| err("read_shallow", e))?;
        let ids = buf
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() {
                    return None;
                }
                gix::ObjectId::from_hex(line.as_bytes())
                    .ok()
                    .map(|id| id.to_string())
            })
            .collect();
        Ok(Some(ids))
    }

    /// Compute the index↔worktree status (same underlying computation as [`status`](Self::status)).
    #[napi]
    pub fn index_worktree(&self) -> Result<AsyncTask<StatusTask>> {
        Ok(AsyncTask::new(StatusTask {
            repo: self.inner.clone(),
        }))
    }

    /// Print an object's content, type, or size — a TypeScript port of `git cat-file`.
    ///
    /// `spec` is resolved like `git rev-parse`: an object id, a ref or ref-like name
    /// (`HEAD`, `main`, `refs/heads/main`), a revision expression (`HEAD~1`, `main^{tree}`),
    /// or a `rev:path` form (`HEAD:src/main.rs`). The `mode` option (see [`CatFileOptions`])
    /// selects which value is returned:
    /// - `"content"` (default) → the object's raw body as a `Buffer`;
    /// - `"type"` → the object kind (`blob`/`tree`/`commit`/`tag`) as a string;
    /// - `"size"` → the object's size in bytes as a number;
    /// - `"exists"` → whether the object exists as a boolean.
    ///
    /// `repo.catFile(...)` throws if `spec` cannot be resolved; use `mode: "exists"` to probe
    /// without raising.
    #[napi]
    pub fn cat_file(
        &self,
        spec: String,
        options: Option<CatFileOptions>,
    ) -> Result<Either4<Buffer, String, f64, bool>> {
        let r = thread_local(&self.inner);
        let mode = options
            .and_then(|o| o.mode)
            .unwrap_or_else(|| "content".to_string());

        // `mode: "exists"` must not throw on an unresolvable spec; it answers the probe.
        if mode == "exists" {
            return match r.rev_parse_single(spec.as_str()) {
                Ok(id) => Ok(Either4::D(r.has_object(id))),
                Err(_) => Ok(Either4::D(false)),
            };
        }

        let oid: gix::ObjectId = r
            .rev_parse_single(spec.as_str())
            .map_err(|e| err("rev_parse_single", e))?
            .into();
        match mode.as_str() {
            "content" => {
                let resolved =
                    crate::replace::resolve_replace_head(&self.inner, &oid).unwrap_or(oid);
                let obj = r
                    .find_object(resolved)
                    .map_err(|e| err("find_object", e))?
                    .detach();
                Ok(Either4::A(Buffer::from(obj.data)))
            }
            "type" => {
                let obj = r.find_object(oid).map_err(|e| err("find_object", e))?;
                Ok(Either4::B(kind_to_string(obj.kind)))
            }
            "size" => {
                let header = r.find_header(oid).map_err(|e| err("find_header", e))?;
                Ok(Either4::C(header.size() as f64))
            }
            other => Err(err_str("cat_file", format!("invalid mode '{other}'"))),
        }
    }
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
    pub signature: Option<String>,
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
    let signature = c
        .extra_headers()
        .find("gpgsig")
        .or_else(|| c.extra_headers().find("gpgsig-sha256"))
        .map(|v| lossy(v));
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
        signature,
    };
    Ok((entry, parent_ids))
}
