use std::sync::Arc;

use napi::Result;
use napi::Task;
use napi::bindgen_prelude::AsyncTask;
use napi_derive::napi;

use crate::error::{err, thread_local};
use crate::repository::Repository;

/// How tags are handled when fetching from a remote.
#[napi]
pub enum FetchTags {
    All,
    None,
    Included,
}

fn to_fetch_tags(t: gix::remote::fetch::Tags) -> FetchTags {
    match t {
        gix::remote::fetch::Tags::All => FetchTags::All,
        gix::remote::fetch::Tags::None => FetchTags::None,
        gix::remote::fetch::Tags::Included => FetchTags::Included,
    }
}

/// Direction for [`Repository::find_default_remote`].
#[napi]
pub enum RemoteDirection {
    /// The remote used for fetching.
    Fetch,
    /// The remote used for pushing.
    Push,
}

pub(crate) fn to_remote_direction(d: RemoteDirection) -> gix::remote::Direction {
    match d {
        RemoteDirection::Fetch => gix::remote::Direction::Fetch,
        RemoteDirection::Push => gix::remote::Direction::Push,
    }
}

/// Acquire a live `gix::Remote` for this binding, reconstructing anonymous remotes from their
/// cached URL, and run `f` against it. The borrowed remote never escapes, avoiding lifetime issues.
fn with_remote<T, F>(
    self_repo: &Arc<gix::ThreadSafeRepository>,
    name: &Option<String>,
    url: &Option<String>,
    f: F,
) -> Result<T>
where
    F: FnOnce(&gix::Remote) -> Result<T>,
{
    let r = thread_local(&self_repo);
    let remote = match name {
        Some(name) => r
            .find_remote(name.as_str())
            .map_err(|e| err("find_remote", e))?,
        None => {
            let url = url
                .as_deref()
                .ok_or_else(|| err("remote", "anonymous remote has no url"))?;
            r.remote_at(url).map_err(|e| err("remote_at", e))?
        }
    };
    f(&remote)
}

/// A configured git remote, exposed to TypeScript.
#[napi]
pub struct Remote {
    repo: Arc<gix::ThreadSafeRepository>,
    name: Option<String>,
    url: Option<String>,
    push_url: Option<String>,
}

#[napi]
impl Remote {
    /// The configured name of this remote, or `null` for an anonymous remote.
    #[napi(getter)]
    pub fn name(&self) -> Option<String> {
        self.name.clone()
    }

    /// The fetch URL, falling back to the push URL if no fetch URL is configured.
    #[napi(getter)]
    pub fn url(&self) -> Option<String> {
        self.url.clone()
    }

    /// The explicit push URL, or `null` if none is configured.
    #[napi(getter)]
    pub fn push_url(&self) -> Option<String> {
        self.push_url.clone()
    }

    /// How tags are handled when fetching from this remote.
    #[napi(getter)]
    pub fn fetch_tags(&self) -> Result<FetchTags> {
        with_remote(&self.repo, &self.name, &self.url, |remote| {
            Ok(to_fetch_tags(remote.fetch_tags()))
        })
    }

    /// The configured fetch refspecs, in configuration order.
    #[napi(getter)]
    pub fn fetch_refspecs(&self) -> Result<Vec<String>> {
        with_remote(&self.repo, &self.name, &self.url, |remote| {
            Ok(remote
                .refspecs(gix::remote::Direction::Fetch)
                .iter()
                .map(|s| s.to_ref().to_bstring().to_string())
                .collect())
        })
    }

    /// The configured push refspecs, in configuration order.
    #[napi(getter)]
    pub fn push_refspecs(&self) -> Result<Vec<String>> {
        with_remote(&self.repo, &self.name, &self.url, |remote| {
            Ok(remote
                .refspecs(gix::remote::Direction::Push)
                .iter()
                .map(|s| s.to_ref().to_bstring().to_string())
                .collect())
        })
    }
    #[napi(ts_return_type = "Promise<FetchOutcome>")]
    pub fn fetch(&self) -> Result<AsyncTask<FetchTask>> {
        Ok(AsyncTask::new(FetchTask {
            repo: Arc::clone(&self.repo),
            name: self.name.clone(),
            url: self.url.clone(),
        }))
    }
}

pub fn build_remote(repo: &Arc<gix::ThreadSafeRepository>, remote: gix::Remote) -> Remote {
    let name = remote
        .name()
        .map(|n| String::from_utf8_lossy(n.as_bstr()).into_owned());
    let url = remote
        .url(gix::remote::Direction::Fetch)
        .or_else(|| remote.url(gix::remote::Direction::Push))
        .map(|u| u.to_string());
    let push_url = remote
        .url(gix::remote::Direction::Push)
        .map(|u| u.to_string());
    Remote {
        repo: Arc::clone(repo),
        name,
        url,
        push_url,
    }
}

/// Summary of a completed [`Remote::fetch`] operation.
#[napi(object)]
pub struct FetchOutcome {
    /// Whether a pack was received (i.e. the remote had new objects for us).
    pub pack_received: bool,
    /// Whether any negotiation with the remote occurred.
    pub negotiated: bool,
}

/// Blocking fetch work, executed on the libuv thread pool by [`AsyncTask`].
pub struct FetchTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) name: Option<String>,
    pub(crate) url: Option<String>,
}

impl Task for FetchTask {
    type Output = FetchOutcome;
    type JsValue = FetchOutcome;

    fn compute(&mut self) -> Result<FetchOutcome> {
        let r = thread_local(&self.repo);
        let remote = match &self.name {
            Some(name) => r
                .find_remote(name.as_str())
                .map_err(|e| err("find_remote", e))?,
            None => {
                let url = self
                    .url
                    .as_deref()
                    .ok_or_else(|| err("remote", "anonymous remote has no url"))?;
                r.remote_at(url).map_err(|e| err("remote_at", e))?
            }
        };
        let conn = remote
            .connect(gix::remote::Direction::Fetch)
            .map_err(|e| err("connect", e))?;
        let options = gix::remote::ref_map::Options::default();
        let prepare = conn
            .prepare_fetch(gix::progress::Discard, options)
            .map_err(|e| err("prepare_fetch", e))?;
        let interrupt = std::sync::atomic::AtomicBool::new(false);
        let outcome = prepare
            .receive(gix::progress::Discard, &interrupt)
            .map_err(|e| err("receive", e))?;
        let pack_received = matches!(outcome.status, gix::remote::fetch::Status::Change { .. });
        let negotiated = match &outcome.status {
            gix::remote::fetch::Status::Change { .. } => true,
            gix::remote::fetch::Status::NoPackReceived { negotiate, .. } => negotiate.is_some(),
        };
        Ok(FetchOutcome {
            pack_received,
            negotiated,
        })
    }

    fn resolve(&mut self, _env: napi::Env, output: FetchOutcome) -> Result<FetchOutcome> {
        Ok(output)
    }
}

#[napi]
impl Repository {
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

    /// Find the default remote for `direction` as configured, or `null` if none is configured.
    #[napi]
    pub fn find_default_remote(&self, direction: RemoteDirection) -> Result<Option<Remote>> {
        let r = thread_local(&self.inner);
        match r.find_default_remote(to_remote_direction(direction)) {
            Some(Ok(remote)) => Ok(Some(build_remote(&self.inner, remote))),
            Some(Err(e)) => Err(err("find_default_remote", e)),
            None => Ok(None),
        }
    }

    /// Find the remote to fetch from given an optional name/URL (emulates `git fetch <name>`).
    #[napi]
    pub fn find_fetch_remote(&self, name_or_url: Option<String>) -> Result<Remote> {
        let r = thread_local(&self.inner);
        let name_bstr = name_or_url
            .as_deref()
            .map(|s| gix::bstr::BStr::new(s.as_bytes()));
        let remote = r
            .find_fetch_remote(name_bstr)
            .map_err(|e| err("find_fetch_remote", e))?;
        Ok(build_remote(&self.inner, remote))
    }

    /// Find a configured remote by name or URL, returning `null` if it does not exist.
    #[napi]
    pub fn try_find_remote(&self, name_or_url: String) -> Result<Option<Remote>> {
        let r = thread_local(&self.inner);
        match r.try_find_remote(name_or_url.as_str()) {
            Some(Ok(remote)) => Ok(Some(build_remote(&self.inner, remote))),
            Some(Err(e)) => Err(err("try_find_remote", e)),
            None => Ok(None),
        }
    }

    /// Like [`try_find_remote`](Self::try_find_remote) but without applying URL rewrite rules.
    #[napi]
    pub fn try_find_remote_without_url_rewrite(
        &self,
        name_or_url: String,
    ) -> Result<Option<Remote>> {
        let r = thread_local(&self.inner);
        match r.try_find_remote_without_url_rewrite(name_or_url.as_str()) {
            Some(ok) => match ok {
                Ok(remote) => Ok(Some(build_remote(&self.inner, remote))),
                Err(e) => Err(err("try_find_remote", e)),
            },
            None => Ok(None),
        }
    }

    /// The names of all configured remotes.
    #[napi]
    pub fn remote_names(&self) -> Vec<String> {
        let r = thread_local(&self.inner);
        r.remote_names()
            .into_iter()
            .map(|n| n.to_string())
            .collect()
    }

    /// The configured default remote name for `direction`, or `null` if none is configured.
    #[napi]
    pub fn remote_default_name(&self, direction: RemoteDirection) -> Option<String> {
        let r = thread_local(&self.inner);
        r.remote_default_name(to_remote_direction(direction))
            .map(|n| n.to_string())
    }

    /// Create an anonymous remote for `url` (URL rewrite rules are applied).
    #[napi]
    pub fn remote_at(&self, url: String) -> Result<Remote> {
        let r = thread_local(&self.inner);
        let remote = r.remote_at(url.as_str()).map_err(|e| err("remote_at", e))?;
        Ok(build_remote(&self.inner, remote))
    }

    /// Create an anonymous remote for `url` without applying URL rewrite rules.
    #[napi]
    pub fn remote_at_without_url_rewrite(&self, url: String) -> Result<Remote> {
        let r = thread_local(&self.inner);
        let remote = r
            .remote_at_without_url_rewrite(url.as_str())
            .map_err(|e| err("remote_at", e))?;
        Ok(build_remote(&self.inner, remote))
    }

    /// Create (or overwrite) a named remote `name` pointing at `url` and return it.
    #[napi]
    pub fn create_remote(&self, name: String, url: String) -> Result<Remote> {
        let r = thread_local(&self.inner);
        // Validate the URL the same way gix would when constructing a remote.
        let _ = r.remote_at(url.as_str()).map_err(|e| err("remote_at", e))?;

        let url_key = format!("remote.{name}.url");
        let fetch_key = format!("remote.{name}.fetch");
        let fetch = format!("+refs/heads/*:refs/remotes/{name}/*");

        // Persist to the on-disk local config (mirrors the proven `config_set` path).
        let local_config_path = r.git_dir().join("config");
        let mut file = gix::config::File::from_path_no_includes(
            local_config_path.clone(),
            gix::config::Source::Local,
        )
        .map_err(|e| err("config_load", e))?;
        file.set_raw_value(&url_key, url.as_str())
            .map_err(|e| err("config_set", e))?;
        file.set_raw_value(&fetch_key, fetch.as_str())
            .map_err(|e| err("config_set", e))?;
        std::fs::write(&local_config_path, file.to_bstring().as_slice())
            .map_err(|e| err("config_write", e))?;

        // Read the persisted config back through a fresh open (always reads disk).
        let open_path = r
            .workdir()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| r.git_dir().to_path_buf());
        let fresh = gix::ThreadSafeRepository::open(&open_path)
            .map_err(|e| err("open", e))?
            .to_thread_local();
        let remote = fresh
            .find_remote(name.as_str())
            .map_err(|e| err("find_remote", e))?;
        Ok(build_remote(&self.inner, remote))
    }
}
