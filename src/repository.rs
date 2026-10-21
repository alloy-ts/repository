use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use napi::Result;
use napi::Task;
use napi::bindgen_prelude::AsyncTask;
use napi_derive::napi;

use crate::error::{cached_namespace_get, cached_namespace_set, err, parse_oid, thread_local};
use crate::object::Signature;

fn lossy(b: &[u8]) -> String {
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

pub(crate) fn signature_from_dto(s: &Signature) -> Result<gix::actor::Signature> {
    Ok(gix::actor::Signature {
        name: s.name.clone().into(),
        email: s.email.clone().into(),
        time: gix::date::Time {
            seconds: s.time,
            offset: s.timezone_offset,
        },
    })
}

/// A single entry in the git index.
#[derive(Clone)]
#[napi(object)]
pub struct IndexEntry {
    pub path: String,
    pub mode: String,
    pub id: String,
    pub stage: u32,
}

/// A single status item for working tree / index.
#[derive(Clone)]
#[napi(object)]
pub struct StatusEntry {
    pub path: String,
    pub status: String,
}

/// A single blame hunk mapping a range of lines in the blamed file to the commit that introduced it.
#[derive(Clone)]
#[napi(object)]
pub struct BlameEntry {
    /// 0-based line index where this hunk begins in the blamed file.
    pub start_in_blamed_file: u32,
    /// 0-based line index where this hunk begins in the source file (may differ if the file moved).
    pub start_in_source_file: u32,
    /// Number of lines this hunk spans.
    pub len: u32,
    /// The id of the commit that introduced this hunk.
    pub commit_id: String,
    /// The source file name, if it differs from the blamed file (renamed file).
    pub source_file_name: Option<String>,
}

/// The result of blaming a file.
#[derive(Clone)]
#[napi(object)]
pub struct BlameOutcome {
    /// The blame hunks, in file order.
    pub entries: Vec<BlameEntry>,
    /// The full content of the blamed file (as it exists at the start commit).
    pub blob: Vec<u8>,
}

/// A git attribute assignment entry.
#[derive(Clone)]
#[napi(object)]
pub struct AttributeEntry {
    pub name: String,
    pub value: String,
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
            let (repo, _outcome) = prepare
                .fetch_only(gix::progress::Discard, &interrupt)
                .ok()?;
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

    /// Open a repository with options.
    #[napi]
    pub fn open_opts(path: String, _bare: Option<bool>) -> Result<Repository> {
        let opts = gix::open::Options::default();
        let ts =
            gix::ThreadSafeRepository::open_opts(path, opts).map_err(|e| err("open_opts", e))?;
        Ok(Repository {
            inner: Arc::new(ts),
        })
    }

    /// Initialize a repository with options.
    #[napi]
    pub fn init_opts(path: String, bare: Option<bool>) -> Result<Repository> {
        Repository::init(path, bare)
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

    /// Check attributes for a relative path.
    #[napi]
    pub fn attributes(&self, relative_path: String) -> Result<Vec<AttributeEntry>> {
        let r = thread_local(&self.inner);
        let index = r
            .index_or_load_from_head_or_empty()
            .map_err(|e| err("index", e))?;
        let mut cache = r
            .attributes(
                &index,
                gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
                gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
                None,
            )
            .map_err(|e| err("attributes", e))?;
        let platform = cache
            .at_path(relative_path.as_str(), None)
            .map_err(|e| err("at_path", e))?;
        let mut out = Vec::new();
        let mut selected = gix::attrs::search::Outcome::default();
        platform.matching_attributes(&mut selected);
        for m in selected.iter() {
            out.push(AttributeEntry {
                name: m.assignment.name.as_str().to_string(),
                value: format!("{:?}", m.assignment.state),
            });
        }
        Ok(out)
    }

    /// Check attributes for a relative path, without applying exclude/ignore information.
    ///
    /// Unlike [`attributes`](Self::attributes), `.gitignore`/`.git/info/exclude` files are
    /// not consulted, so this is the appropriate query when only `.gitattributes` matches
    /// are relevant.
    #[napi]
    pub fn attributes_only(&self, relative_path: String) -> Result<Vec<AttributeEntry>> {
        let r = thread_local(&self.inner);
        let index = r
            .index_or_load_from_head_or_empty()
            .map_err(|e| err("index", e))?;
        let mut cache = r
            .attributes_only(
                &index,
                gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
            )
            .map_err(|e| err("attributes_only", e))?;
        let platform = cache
            .at_path(relative_path.as_str(), None)
            .map_err(|e| err("at_path", e))?;
        let mut out = Vec::new();
        let mut selected = gix::attrs::search::Outcome::default();
        platform.matching_attributes(&mut selected);
        for m in selected.iter() {
            out.push(AttributeEntry {
                name: m.assignment.name.as_str().to_string(),
                value: format!("{:?}", m.assignment.state),
            });
        }
        Ok(out)
    }

    /// Check whether a relative path is excluded by gitignore / exclude files.
    #[napi]
    pub fn is_path_excluded(&self, relative_path: String) -> Result<bool> {
        let r = thread_local(&self.inner);
        let index = r
            .index_or_load_from_head_or_empty()
            .map_err(|e| err("index", e))?;
        let mut cache = r
            .excludes(
                &index,
                None,
                gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
            )
            .map_err(|e| err("excludes", e))?;
        let platform = cache
            .at_path(
                relative_path.as_str(),
                Some(gix::objs::tree::EntryKind::Blob.into()),
            )
            .map_err(|e| err("at_path", e))?;
        Ok(platform.is_excluded())
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

    /// Retrieve default author signature configured for this repository.
    #[napi]
    pub fn default_author(&self) -> Result<Option<Signature>> {
        let r = thread_local(&self.inner);
        match r.author() {
            Some(Ok(s)) => Ok(Some(Signature {
                name: lossy(s.name),
                email: lossy(s.email),
                time: s.time().ok().map(|t| t.seconds).unwrap_or(0),
                timezone_offset: s.time().ok().map(|t| t.offset).unwrap_or(0),
            })),
            _ => Ok(None),
        }
    }

    /// Retrieve default committer signature configured for this repository.
    #[napi]
    pub fn default_committer(&self) -> Result<Option<Signature>> {
        let r = thread_local(&self.inner);
        match r.committer() {
            Some(Ok(s)) => Ok(Some(Signature {
                name: lossy(s.name),
                email: lossy(s.email),
                time: s.time().ok().map(|t| t.seconds).unwrap_or(0),
                timezone_offset: s.time().ok().map(|t| t.offset).unwrap_or(0),
            })),
            _ => Ok(None),
        }
    }

    /// Return the configured committer, or install `name`/`email` as an in-memory
    /// fallback if none is configured, and return it.
    ///
    /// The fallback lives only on the underlying handle for the duration of this call;
    /// it is not persisted to configuration.
    #[napi]
    pub fn committer_or_set_fallback(&self, name: String, email: String) -> Result<Signature> {
        let mut r = thread_local(&self.inner);
        let s = r
            .committer_or_set_fallback(name.as_bytes(), email.as_bytes())
            .map_err(|e| err("committer_or_set_fallback", e))?;
        Ok(Signature {
            name: lossy(s.name),
            email: lossy(s.email),
            time: s.time().ok().map(|t| t.seconds).unwrap_or(0),
            timezone_offset: s.time().ok().map(|t| t.offset).unwrap_or(0),
        })
    }

    /// Like `committerOrSetFallback`, but installs a generic fallback identity when
    /// nothing is configured.
    #[napi]
    pub fn committer_or_set_generic_fallback(&self) -> Result<Signature> {
        let mut r = thread_local(&self.inner);
        let s = r
            .committer_or_set_generic_fallback()
            .map_err(|e| err("committer_or_set_generic_fallback", e))?;
        Ok(Signature {
            name: lossy(s.name),
            email: lossy(s.email),
            time: s.time().ok().map(|t| t.seconds).unwrap_or(0),
            timezone_offset: s.time().ok().map(|t| t.offset).unwrap_or(0),
        })
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

    /// Compute a blame for `file_path` (relative to the repository root), starting from
    /// `commit_id` (or `HEAD` if omitted). Returns the per-hunk blame entries plus the full
    /// content of the blamed file.
    #[napi]
    pub fn blame(&self, file_path: String, commit_id: Option<String>) -> Result<BlameOutcome> {
        let r = thread_local(&self.inner);
        let suspect = match commit_id {
            Some(c) => parse_oid(&c)?,
            None => r.head_commit().map_err(|e| err("head_commit", e))?.id,
        };
        let outcome = r
            .blame_file(
                gix::bstr::BStr::new(file_path.as_str()),
                suspect,
                Default::default(),
            )
            .map_err(|e| err("blame_file", e))?;

        let mut entries = Vec::with_capacity(outcome.entries.len());
        for e in &outcome.entries {
            entries.push(BlameEntry {
                start_in_blamed_file: e.start_in_blamed_file,
                start_in_source_file: e.start_in_source_file,
                len: e.len.get(),
                commit_id: e.commit_id.to_string(),
                source_file_name: e.source_file_name.as_ref().map(|n| lossy(n)),
            });
        }
        Ok(BlameOutcome {
            entries,
            blob: outcome.blob,
        })
    }

    /// Return the currently set reference namespace, if any.
    ///
    /// The value is cached per repository because each synchronous call gets a fresh
    /// thread-local view whose in-memory namespace would not otherwise persist.
    #[napi]
    pub fn namespace(&self) -> Option<String> {
        cached_namespace_get(Arc::as_ptr(&self.inner) as usize)
    }

    /// Set the reference namespace and return the previously set one (if any).
    #[napi]
    pub fn set_namespace(&self, ns: String) -> Result<Option<String>> {
        let key = Arc::as_ptr(&self.inner) as usize;
        let previous = cached_namespace_get(key);
        let mut r = thread_local(&self.inner);
        r.set_namespace(ns.as_str())
            .map_err(|e| err("set_namespace", e))?;
        // Compute the canonical `refs/namespaces/<name>/` form directly (a fresh
        // thread-local view would not otherwise report the value we just set).
        let current = gix::refs::namespace::expand(ns.as_str())
            .map(|n| String::from_utf8_lossy(n.into_bstring().as_slice()).into_owned())
            .map_err(|e| err("namespace_expand", e))?;
        cached_namespace_set(key, Some(current));
        Ok(previous)
    }

    /// Clear the reference namespace and return the previously set one (if any).
    #[napi]
    pub fn clear_namespace(&self) -> Result<Option<String>> {
        let key = Arc::as_ptr(&self.inner) as usize;
        let previous = cached_namespace_get(key);
        let mut r = thread_local(&self.inner);
        r.clear_namespace();
        cached_namespace_set(key, None);
        Ok(previous)
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

    /// Produce a `git describe` string for `id` using all refs as candidate names.
    #[napi]
    pub fn describe(&self, id: String) -> Result<Option<String>> {
        let r = thread_local(&self.inner);
        let oid = parse_oid(&id)?;
        let commit = r.find_commit(oid).map_err(|e| err("find_commit", e))?;
        let describe_out = commit
            .describe()
            .names(gix::commit::describe::SelectRef::AllRefs)
            .try_format()
            .map_err(|e| err("describe_format", e))?;
        Ok(describe_out.map(|f| f.to_string()))
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
