use std::sync::Arc;

use napi::Result;
use napi::Task;
use napi::bindgen_prelude::AsyncTask;
use napi_derive::napi;

use crate::error::{err, thread_local};

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
fn with_remote<T, F>(self_repo: &Arc<gix::ThreadSafeRepository>, name: &Option<String>, url: &Option<String>, f: F) -> Result<T>
where
    F: FnOnce(&gix::Remote) -> Result<T>,
{
    let r = thread_local(&self_repo);
    let remote = match name {
        Some(name) => r.find_remote(name.as_str()).map_err(|e| err("find_remote", e))?,
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
