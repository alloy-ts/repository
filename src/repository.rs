use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use napi::bindgen_prelude::{Buffer, Either4};
use napi_derive::napi;

use crate::error::{
    cached_namespace_get, cached_namespace_set, err, err_str, parse_oid, thread_local,
};
use crate::object::kind_to_string;
use crate::reference::Reference;

pub(crate) fn current_repo() -> napi::Result<Arc<gix::ThreadSafeRepository>> {
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
pub(crate) fn find_git() -> Option<std::path::PathBuf> {
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

/// Result of [`Repository::ahead_behind`]: how far `HEAD` is ahead of and behind `upstream`.
#[napi(object)]
pub struct AheadBehind {
    /// Commits in `HEAD` not reachable from `upstream`.
    pub ahead: u32,
    /// Commits in `upstream` not reachable from `HEAD`.
    pub behind: u32,
}

/// The primary handle to a git repository, exposed to TypeScript as a class.
///
/// Internally wraps `Arc<gix::ThreadSafeRepository>` so it can be shared and offloaded to worker
/// threads for async network operations, while sync methods obtain a cheap thread-local view.
///
/// This is a *handle-based* API (open a `Repository` once, then call methods on it) rather than
/// the stateless, path-per-call style some git2 wrappers use. The equivalents of common
/// path-based helpers are:
///
/// | Path-based helper | `Repository` method (sync unless noted) |
/// |---|---|
/// | `is_repo(dir)` | `Repository.isRepo(dir)` (static) |
/// | `is_dirty(dir)` | `repo.isDirty()` (async Task) |
/// | `get_current_hash(dir)` | `repo.headId()` |
/// | `rev_as_hash(dir, rev)` | `repo.revToHash(rev)` / `repo.revParse(rev)` |
/// | `clone_repo(url, dir)` | `Repository.clone(url, dir?)` (async) / `Cloner` |
/// | `checkout(hash, dir)` | `repo.checkout(rev, opts?)` (async) |
/// | `fetch()` | `repo.fetch(remoteName?, opts?)` (async) / `Remote.fetch` |
/// | `credentials_callback` | `Auth` passed via `auth` option |
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

pub(crate) fn to_gix_open_options(o: OpenOptions) -> gix::open::Options {
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
    pub fn new(path: Option<String>) -> napi::Result<Repository> {
        match path {
            Some(p) => Self::open(p),
            None => Self::discover(None),
        }
    }

    /// Discover a repository by walking up from `path` (or the current directory if omitted).
    #[napi]
    pub fn discover(path: Option<String>) -> napi::Result<Repository> {
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
    ) -> napi::Result<Repository> {
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
    ) -> napi::Result<Repository> {
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

    /// Open the repository at `path` (expands `path/.git` for worktrees).
    #[napi]
    pub fn open(path: String) -> napi::Result<Repository> {
        let ts = gix::ThreadSafeRepository::open(path).map_err(|e| err("open", e))?;
        Ok(Repository {
            inner: Arc::new(ts),
        })
    }

    /// Whether `path` is a git repository (opens exactly that location; like the
    /// reference's `is_repo(dir)`). Returns `false` instead of throwing when the
    /// path is not a repository or cannot be opened.
    #[napi]
    pub fn is_repo(path: String) -> bool {
        gix::ThreadSafeRepository::open(path).is_ok()
    }

    /// Open a repository with the given `options` (trust, config overrides, strict/lossy config, …).
    #[napi]
    pub fn open_opts(path: String, options: OpenOptions) -> napi::Result<Repository> {
        let ts = gix::ThreadSafeRepository::open_opts(path, to_gix_open_options(options))
            .map_err(|e| err("open_opts", e))?;
        Ok(Repository {
            inner: Arc::new(ts),
        })
    }

    /// The working directory, if this is not a bare repository.
    #[napi]
    pub fn work_dir(&self) -> Option<String> {
        let r = thread_local(&self.inner);
        r.workdir().map(|p| p.to_string_lossy().into_owned())
    }

    /// Return the main repository if this is a linked worktree.
    #[napi]
    pub fn main_repository(&self) -> napi::Result<Repository> {
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
    pub fn filesystem_options(&self) -> napi::Result<FsCapabilities> {
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
    pub fn stat_options(&self) -> napi::Result<StatOptions> {
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
    pub fn install_dir(&self) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        r.install_dir()
            .map(|p| p.display().to_string())
            .map_err(|e| err("install_dir", e))
    }

    /// The relative path between the working tree and the current working dir, or `null`.
    #[napi]
    pub fn prefix(&self) -> napi::Result<Option<String>> {
        let r = thread_local(&self.inner);
        Ok(r.prefix()
            .map_err(|e| err("prefix", e))?
            .map(|p| p.display().to_string()))
    }

    /// Normalize `path` into a repository-relative Git path with slash separators.
    #[napi]
    pub fn normalize_path(&self, path: String) -> napi::Result<String> {
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
    pub fn set_workdir(&self, workdir: Option<String>) -> napi::Result<Option<String>> {
        let mut r = thread_local(&self.inner);
        let previous = r
            .set_workdir(workdir.map(std::path::PathBuf::from))
            .map_err(|e| err("set_workdir", e))?;
        Ok(previous.map(|p| p.display().to_string()))
    }

    /// Return true if `ancestor` is an ancestor of (or equal to) `descendant`.
    #[napi]
    pub fn is_ancestor(&self, ancestor: String, descendant: String) -> napi::Result<bool> {
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
    pub fn is_shallow(&self) -> napi::Result<bool> {
        let r = thread_local(&self.inner);
        r.is_shallow().map_err(|e| err("is_shallow", e))
    }

    /// Path to the `shallow` file (may not exist if not a shallow clone).
    #[napi]
    pub fn shallow_file(&self) -> napi::Result<String> {
        let r = thread_local(&self.inner);
        let path = r.shallow_file().map_err(|e| err("shallow_file", e))?;
        Ok(path.to_string_lossy().into_owned())
    }

    /// List the shallow boundary commits, or `null` if the repo is not shallow.
    #[napi]
    pub fn shallow_commits(&self) -> napi::Result<Option<Vec<String>>> {
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
    /// Whether the repository has no commits yet (HEAD cannot resolve to a commit object).
    ///
    /// Mirrors git2's `git_repository_is_empty`.
    #[napi]
    pub fn is_empty(&self) -> napi::Result<bool> {
        let r = thread_local(&self.inner);
        match r.head_commit() {
            Ok(_) => Ok(false),
            Err(_) => Ok(true),
        }
    }

    /// Read the in-progress commit message (`.git/COMMIT_EDITMSG`), or `null` when none is
    /// pending. Mirrors git2's `git_repository_message`.
    #[napi]
    pub fn message(&self) -> napi::Result<Option<String>> {
        let r = thread_local(&self.inner);
        let p = r.git_dir().join("COMMIT_EDITMSG");
        match std::fs::read_to_string(&p) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(err("message", e)),
        }
    }

    /// Remove the in-progress commit message file. Mirrors git2's
    /// `git_repository_message_remove`.
    #[napi]
    pub fn remove_message(&self) -> napi::Result<()> {
        let r = thread_local(&self.inner);
        let p = r.git_dir().join("COMMIT_EDITMSG");
        match std::fs::remove_file(&p) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(err("remove_message", e)),
        }
    }

    /// Disambiguate an abbreviated object id prefix to a full object id, or `null` if no unique
    /// object matches. Mirrors git2's `git_object_id_by_prefix` / `find_object_by_prefix`.
    #[napi]
    pub fn find_object_by_prefix(&self, prefix: String) -> napi::Result<Option<String>> {
        Ok(self.rev_parse_single(prefix).ok())
    }

    /// Like [`find_object_by_prefix`](Self::find_object_by_prefix), but only returns the id when
    /// the resolved object is a commit.
    #[napi]
    pub fn find_commit_by_prefix(&self, prefix: String) -> napi::Result<Option<String>> {
        self.find_object_by_prefix_of_kind(prefix, gix::object::Kind::Commit)
    }

    /// Like [`find_object_by_prefix`](Self::find_object_by_prefix), but only returns the id when
    /// the resolved object is a tag.
    #[napi]
    pub fn find_tag_by_prefix(&self, prefix: String) -> napi::Result<Option<String>> {
        self.find_object_by_prefix_of_kind(prefix, gix::object::Kind::Tag)
    }

    /// Resolve `spec` to a full object id string, or `null` if it cannot be resolved.
    ///
    /// Thin, non-throwing convenience over [`rev_parse_single`](Self::rev_parse_single)
    /// for the common "rev → hash" case (mirrors the reference's `rev_as_hash`). Unlike
    /// `rev_parse_single`, this returns `null` rather than throwing on an unresolvable spec.
    #[napi]
    pub fn rev_to_hash(&self, spec: String) -> napi::Result<Option<String>> {
        Ok(self.rev_parse_single(spec).ok())
    }

    fn find_object_by_prefix_of_kind(
        &self,
        prefix: String,
        want: gix::object::Kind,
    ) -> napi::Result<Option<String>> {
        let id = match self.rev_parse_single(prefix) {
            Ok(id) => id,
            Err(_) => return Ok(None),
        };
        let r = thread_local(&self.inner);
        let oid = parse_oid(&id)?;
        match r.find_object(oid) {
            Ok(obj) if obj.kind == want => Ok(Some(id)),
            _ => Ok(None),
        }
    }

    /// Open the repository named by the `GIT_DIR` (or `REPO_DIR`) environment variable, falling
    /// back to discovery of the current directory. Mirrors git2's `git_repository_open_from_env`.
    #[napi]
    pub fn open_from_env() -> napi::Result<Repository> {
        let ts = current_repo()?;
        Ok(Repository { inner: ts })
    }

    #[napi]
    pub fn cat_file(
        &self,
        spec: String,
        options: Option<CatFileOptions>,
    ) -> napi::Result<Either4<Buffer, String, f64, bool>> {
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

    /// Resolve references whose name starts with `pattern` (a prefix like `refs/heads/`).
    ///
    /// When `pattern` is omitted, returns every reference (identical to `references()`).
    /// This is the napi equivalent of the reference service layer's `ref_list(pattern)`.
    #[napi(ts_return_type = "Reference[]")]
    pub fn references_matching(&self, pattern: Option<String>) -> napi::Result<Vec<Reference>> {
        match pattern {
            Some(p) => self.references_prefixed(p),
            None => self.references(),
        }
    }

    /// Read a single config value as a boolean (`true`/`false`), or `null` when unset or not
    /// parseable as a boolean. Mirrors the reference service's `config_get_bool`.
    #[napi]
    pub fn config_bool(&self, key: String) -> Option<bool> {
        let r = thread_local(&self.inner);
        r.config_snapshot().boolean(key.as_str())
    }

    /// Create or update a reference `name` to point at object `target` (a sha or a ref name that
    /// resolves to one). Mirrors the reference service's `ref_update`. Throws if `target` is not a
    /// resolvable object id.
    #[napi]
    pub fn ref_update(&self, name: String, target: String) -> napi::Result<()> {
        let r = thread_local(&self.inner);
        let oid = parse_oid(&target)?;
        r.reference(
            name.as_str(),
            oid,
            gix::refs::transaction::PreviousValue::Any,
            format!("update {name}"),
        )
        .map_err(|e| err("ref_update", e))?;
        Ok(())
    }

    /// Stage every file in the worktree into the index, like `git add -A`. This is a convenience
    /// wrapper over `Index.add_all` + `Index.write`; bare repositories are a no-op.
    #[napi]
    pub fn stage_all(&self) -> napi::Result<()> {
        let mut idx = self.index()?;
        self.add_all(&mut idx)?;
        idx.write(None)?;
        Ok(())
    }

    /// Count the commits `HEAD` is ahead of and behind `upstream` (a sha or a ref name).
    /// Mirrors the reference service's `ahead_behind(upstream)`.
    #[napi]
    pub fn ahead_behind(&self, upstream: String) -> napi::Result<AheadBehind> {
        let r = thread_local(&self.inner);
        let head = r.head_commit().map_err(|e| err("head_commit", e))?;
        let head_id = head.id;
        let upstream_oid = match parse_oid(&upstream) {
            Ok(oid) => oid,
            Err(_) => {
                let mut reference = r
                    .find_reference(upstream.as_str())
                    .map_err(|e| err("find_reference", e))?;
                reference
                    .peel_to_id()
                    .map_err(|e| err("peel_to_id", e))?
                    .detach()
            }
        };
        let base = match r
            .merge_base(head_id, upstream_oid)
            .map_err(|e| err("merge_base", e))?
        {
            Some(b) => b.detach(),
            None => {
                let ahead = count_from(&r, head_id, None)?;
                let behind = count_from(&r, upstream_oid, None)?;
                return Ok(AheadBehind { ahead, behind });
            }
        };
        let ahead = count_from(&r, head_id, Some(base))?;
        let behind = count_from(&r, upstream_oid, Some(base))?;
        Ok(AheadBehind { ahead, behind })
    }
}

/// Count commits reachable from `from`, stopping (exclusively) at `stop`.
fn count_from(
    r: &gix::Repository,
    from: gix::ObjectId,
    stop: Option<gix::ObjectId>,
) -> napi::Result<u32> {
    let commit = r.find_commit(from).map_err(|e| err("find_commit", e))?;
    let walk = commit
        .ancestors()
        .selected(move |oid| match stop {
            Some(s) => oid.as_bytes() != s.as_bytes(),
            None => true,
        })
        .map_err(|e| err("ancestors", e))?;
    Ok(walk.filter_map(|r| r.ok()).count() as u32)
}
