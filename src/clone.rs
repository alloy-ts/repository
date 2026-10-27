use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use napi::Task;
use napi::bindgen_prelude::{AsyncTask, Function};
use napi::threadsafe_function::ThreadsafeFunction;
use napi_derive::napi;

use crate::auth::{Auth, AuthOpts};
use crate::progress::{ProgressCallback, ProgressEvent};
use crate::repository::{Repository, find_git};

/// Blocking clone work, executed on the libuv thread pool by [`AsyncTask`].
pub struct CloneTask {
    url: String,
    path: String,
    bare: bool,
    progress: Option<Arc<ThreadsafeFunction<ProgressEvent, ()>>>,
    auth: Option<AuthOpts>,
    depth: Option<u32>,
    filter: Option<String>,
    /// Branch to check out (`git clone -b`); `None` checks out the remote's default branch.
    branch: Option<String>,
    /// Restrict the fetch to the chosen branch (`git clone --single-branch`).
    single_branch: Option<bool>,
}

impl Task for CloneTask {
    type Output = Repository;
    type JsValue = Repository;

    fn compute(&mut self) -> napi::Result<Repository> {
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
        let depth = self.depth;
        let filter = self.filter.clone();

        // When an authenticator is supplied, use the git subprocess (which applies the auth env /
        // config) rather than gix, since gix's credential wiring is only best-effort. git still
        // clones local targets natively.
        if let Some(auth) = self.auth.take() {
            if let Some(git) = find_git() {
                let _ = std::fs::remove_dir_all(&path);
                let mut cmd = std::process::Command::new(&git);
                cmd.arg("clone");
                if bare {
                    cmd.arg("--bare");
                }
                if let Some(d) = depth {
                    cmd.arg("--depth").arg(d.to_string());
                }
                if let Some(f) = &filter {
                    cmd.arg(format!("--filter={f}"));
                }
                if let Some(b) = &self.branch {
                    cmd.arg("-b").arg(b);
                }
                if self.single_branch.unwrap_or(false) {
                    cmd.arg("--single-branch");
                }
                Auth::new(Some(auth)).apply_to_command(&mut cmd);
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
                            "authenticated git clone failed: {}",
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
            return Err(napi::Error::from_reason(
                "authenticated clone failed: no system git binary was found",
            ));
        }

        // Try a gix-native clone first. gix can't honor depth/filter here, so route those to git.
        let use_git_for_depth = depth.is_some() || filter.is_some();
        let gix_repo = if use_git_for_depth {
            None
        } else {
            (|| {
                let mut prepare = if bare {
                    gix::prepare_clone_bare(url.as_str(), &path)
                } else {
                    gix::prepare_clone(url.as_str(), &path)
                }
                .ok()?;
                if let Some(branch) = &self.branch {
                    prepare = prepare.with_branch(branch.as_str());
                }
                let interrupt = AtomicBool::new(false);
                let progress = ProgressCallback::new(self.progress.clone());
                let (repo, _outcome) = prepare.fetch_only(progress, &interrupt).ok()?;
                Some(repo.into())
            })()
        };

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
            if let Some(d) = depth {
                cmd.arg("--depth").arg(d.to_string());
            }
            if let Some(f) = &filter {
                cmd.arg(format!("--filter={f}"));
            }
            if let Some(b) = &self.branch {
                cmd.arg("-b").arg(b);
            }
            if self.single_branch.unwrap_or(false) {
                cmd.arg("--single-branch");
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

    fn resolve(&mut self, _env: napi::Env, output: Repository) -> napi::Result<Repository> {
        Ok(output)
    }
}

#[napi]
impl Repository {
    /// Clone a repository from `url` into `path`.
    ///
    /// When `path` is omitted, the destination defaults to a sanitized repository name
    /// derived from `url` in the current working directory (like `git clone <url>`).
    /// Async: the (blocking) network transfer runs on a libuv worker thread and resolves a
    /// `Promise<Repository>`.
    ///
    /// Advanced options (bare, depth, filter, progress, auth) are passed via the trailing
    /// `CloneOptions` object, mirroring `Cloner.clone`. Unlike the previous positional
    /// signature, `depth`/`filter` are now honored through this entry point as well.
    ///
    /// Errors if `path` already exists (mirrors `git clone`).
    #[napi(ts_return_type = "Promise<Repository>")]
    pub fn clone(
        url: String,
        path: Option<String>,
        options: Option<CloneOptions>,
    ) -> napi::Result<AsyncTask<CloneTask>> {
        let options = options.unwrap_or_default();
        let destination = match path {
            Some(p) => p,
            None => default_destination_name(&url)?,
        };
        if std::path::Path::new(&destination).exists() {
            return Err(napi::Error::from_reason(format!(
                "destination already exists: {destination}"
            )));
        }
        let auth = options.auth.clone();
        let tsfn = match options.progress {
            Some(f) => {
                let builder = f.build_threadsafe_function::<ProgressEvent>();
                Some(Arc::new(builder.callee_handled().build()?))
            }
            None => None,
        };
        Ok(AsyncTask::new(CloneTask {
            url,
            path: destination,
            bare: options.bare.unwrap_or(false),
            progress: tsfn,
            auth,
            depth: options.depth,
            filter: options.filter,
            branch: options.branch.clone(),
            single_branch: options.single_branch,
        }))
    }

    /// Validate that `url` looks like a cloneable Git URL. Pure helper, also available
    /// on [`Cloner`].
    #[napi]
    pub fn is_valid_git_url(url: String) -> bool {
        is_valid_git_url(&url)
    }

    /// Extract and sanitize a repository name from `url` (used as the default clone
    /// destination). Throws on an invalid URL. Pure helper, also available on [`Cloner`].
    #[napi]
    pub fn extract_repo_name(url: String) -> napi::Result<String> {
        extract_repo_name(&url)
    }

    /// Convert an SSH URL to its HTTPS equivalent, or `null` if `url` is not SSH.
    /// Pure helper, also available on [`Cloner`].
    #[napi]
    pub fn convert_ssh_to_https(url: String) -> Option<String> {
        convert_ssh_to_https(&url)
    }
}

/// Options for [`Cloner::clone`].
#[napi(object)]
#[derive(Default)]
pub struct CloneOptions {
    /// Create a bare repository (no worktree). Defaults to `false`.
    pub bare: Option<bool>,
    /// Progress callback receiving [`ProgressEvent`]s during the fetch.
    pub progress: Option<Function<ProgressEvent, ()>>,
    /// Per-call authenticator; overrides any authenticator configured on the `Cloner`.
    pub auth: Option<AuthOpts>,
    /// Create a shallow clone with at most this many commits of history. `None` (default) clones
    /// the full history. Honored via the system `git` binary (gix cannot easily apply it here).
    pub depth: Option<u32>,
    /// Partial-clone filter (e.g. `blob:none`), passed to `git clone --filter=`. `None` by default.
    pub filter: Option<String>,
    /// Branch to check out, mirroring `git clone -b <branch>` (e.g. `main` or `release/1.0`).
    /// `None` checks out the remote's default branch.
    pub branch: Option<String>,
    /// Restrict the fetch to the chosen branch (`git clone --single-branch`). When `branch` is set
    /// this fetches only that branch's history; `None` (default) fetches all branches.
    pub single_branch: Option<bool>,
}

/// A configurable clone entry point, mirroring the ergonomics of `git clone`.
///
/// Unlike the low-level [`Repository::clone`], a `Cloner` carries an authenticator and
/// lets `path` be omitted — the destination then defaults to a sanitized repository name
/// derived from `url`, exactly like `git clone <url>`.
#[napi]
pub struct Cloner {
    auth: Option<AuthOpts>,
}

#[napi]
impl Cloner {
    /// Create a `Cloner`. `auth` is applied to every clone unless overridden per call.
    #[napi(constructor)]
    pub fn new(auth: Option<AuthOpts>) -> Self {
        Self { auth }
    }

    /// Clone `url` into `path`.
    ///
    /// When `path` is omitted, the destination defaults to a sanitized repository name
    /// derived from `url` in the current working directory. Async: the blocking network
    /// transfer runs on a libuv worker thread and resolves a `Promise<Repository>`.
    ///
    /// Errors if `path` already exists (mirrors `git clone` and the classic `Cloner`).
    #[napi(ts_return_type = "Promise<Repository>")]
    pub fn clone(
        &self,
        url: String,
        path: Option<String>,
        options: Option<CloneOptions>,
    ) -> napi::Result<AsyncTask<CloneTask>> {
        let options = options.unwrap_or_default();
        let destination = match path {
            Some(p) => p,
            None => default_destination_name(&url)?,
        };
        if std::path::Path::new(&destination).exists() {
            return Err(napi::Error::from_reason(format!(
                "destination already exists: {destination}"
            )));
        }
        let auth = options.auth.clone().or_else(|| self.auth.clone());
        let tsfn = match options.progress {
            Some(f) => {
                let builder = f.build_threadsafe_function::<ProgressEvent>();
                Some(Arc::new(builder.callee_handled().build()?))
            }
            None => None,
        };
        Ok(AsyncTask::new(CloneTask {
            url,
            path: destination,
            bare: options.bare.unwrap_or(false),
            progress: tsfn,
            auth,
            depth: options.depth,
            filter: options.filter,
            branch: options.branch.clone(),
            single_branch: options.single_branch,
        }))
    }

    /// Validate that `url` looks like a cloneable Git URL.
    #[napi]
    pub fn is_valid_git_url(url: String) -> bool {
        is_valid_git_url(&url)
    }

    /// Extract and sanitize a repository name from `url` (used as the default destination).
    /// Throws on an invalid URL.
    #[napi]
    pub fn extract_repo_name(url: String) -> napi::Result<String> {
        extract_repo_name(&url)
    }

    /// Convert an SSH URL to its HTTPS equivalent, or `null` if `url` is not SSH.
    #[napi]
    pub fn convert_ssh_to_https(url: String) -> Option<String> {
        convert_ssh_to_https(&url)
    }
}

/// Validate that `url` looks like a cloneable Git URL (protocol-aware, matching git's
/// accepted schemes). Pure helper backing [`Cloner::is_valid_git_url`].
fn is_valid_git_url(url: &str) -> bool {
    let has_valid_protocol = url.starts_with("https://")
        || url.starts_with("http://")
        || url.starts_with("git@")
        || url.starts_with("ssh://")
        || url.starts_with("git://")
        || (url.starts_with("file://") && url.len() > 7);
    if !has_valid_protocol {
        return false;
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        let parts: Vec<&str> = url.split('/').collect();
        // Should have at least: ["https:", "", "host", "user", "repo"].
        if parts.len() < 5 {
            return false;
        }
        let last_part = parts.last().unwrap_or(&"").trim_end_matches(".git");
        if last_part.is_empty() {
            return false;
        }
    }
    // For SSH URLs like git@github.com:user/repo.git require both a ':' and a '/'.
    if url.starts_with("git@") && (!url.contains(':') || !url.contains('/')) {
        return false;
    }
    true
}

/// Extract and sanitize a repository name from a Git `url`, validating URL syntax first
/// (throws on an invalid URL). This is the strict, URL-only variant exposed for callers that
/// want validation; the automatic destination used when `path` is omitted is derived by the
/// more lenient [`default_destination_name`], which also accepts bare local filesystem paths.
fn extract_repo_name(url: &str) -> napi::Result<String> {
    let url = url.trim_end_matches('/');
    if !is_valid_git_url(url) {
        return Err(napi::Error::from_reason(format!("invalid git url: {url}")));
    }
    let name = url
        .split('/')
        .next_back()
        .ok_or_else(|| napi::Error::from_reason(format!("invalid git url: {url}")))?
        .trim_end_matches(".git");
    if name.is_empty() {
        return Err(napi::Error::from_reason(format!("invalid git url: {url}")));
    }
    let sanitized: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect();
    if sanitized.is_empty() {
        return Err(napi::Error::from_reason(format!("invalid git url: {url}")));
    }
    Ok(sanitized)
}

/// Derive a default clone destination name from `url`, handling both Git URLs and bare local
/// filesystem paths (like `git clone <url>`). Strips a trailing `.git` and sanitizes the
/// result. This is intentionally more lenient than [`extract_repo_name`] so that local-path
/// clones also get a sensible default directory name.
fn default_destination_name(url: &str) -> napi::Result<String> {
    let trimmed = url.trim_end_matches('/');
    let last = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let name = last.trim_end_matches(".git");
    if name.is_empty() {
        return Err(napi::Error::from_reason(format!(
            "cannot derive a repository name from: {url}"
        )));
    }
    let sanitized: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect();
    if sanitized.is_empty() {
        return Err(napi::Error::from_reason(format!(
            "cannot derive a repository name from: {url}"
        )));
    }
    Ok(sanitized)
}

/// Convert an SSH URL (`git@host:user/repo` or `ssh://git@host/user/repo`) to its HTTPS
/// equivalent; returns `None` for non-SSH URLs. Pure helper backing
/// [`Cloner::convert_ssh_to_https`].
fn convert_ssh_to_https(url: &str) -> Option<String> {
    // Handle git@host:user/repo.git format.
    if url.starts_with("git@") && url.contains(':') && !url.starts_with("ssh://") {
        if let Some((host_part, path_part)) = url.split_once(':') {
            let host = host_part.strip_prefix("git@")?;
            return Some(format!("https://{host}/{path_part}"));
        }
    }
    // Handle ssh://git@host/user/repo.git format.
    if url.starts_with("ssh://") {
        if let Some(ssh_url) = url.strip_prefix("ssh://") {
            if let Some(host_and_path) = ssh_url.strip_prefix("git@") {
                if let Some((host, path)) = host_and_path.split_once('/') {
                    return Some(format!("https://{host}/{path}"));
                }
            }
        }
    }
    None
}
