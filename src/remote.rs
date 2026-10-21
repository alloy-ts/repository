use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use napi::Result;
use napi::Task;
use napi::bindgen_prelude::AsyncTask;
use napi_derive::napi;

use crate::error::{err, thread_local};
use crate::repository::Repository;

/// The result of a `fetch`: local refs updated by the fetch.
#[derive(Clone)]
#[napi(object)]
pub struct RemoteFetchOutcome {
    /// Local ref names that were updated by the fetch.
    pub updated_refs: Vec<String>,
}

/// A remote configured for this repository.
#[napi]
pub struct Remote {
    #[napi(readonly)]
    pub name: Option<String>,
    #[napi(readonly)]
    pub url: Option<String>,
    repo: Arc<gix::ThreadSafeRepository>,
}

pub(crate) fn build_remote(repo: &Arc<gix::ThreadSafeRepository>, remote: gix::Remote) -> Remote {
    let name = remote
        .name()
        .map(|n| String::from_utf8_lossy(n.as_bstr()).into_owned());
    let url = remote
        .url(gix::remote::Direction::Fetch)
        .map(|u| u.to_string());
    Remote {
        name,
        url,
        repo: Arc::clone(repo),
    }
}

/// Blocking fetch work, executed on the libuv thread pool by [`AsyncTask`].
pub struct FetchTask {
    repo: Arc<gix::ThreadSafeRepository>,
    name: String,
}

impl Task for FetchTask {
    type Output = RemoteFetchOutcome;
    type JsValue = RemoteFetchOutcome;

    fn compute(&mut self) -> Result<RemoteFetchOutcome> {
        do_fetch(Arc::clone(&self.repo), self.name.clone())
    }

    fn resolve(
        &mut self,
        _env: napi::Env,
        output: RemoteFetchOutcome,
    ) -> Result<RemoteFetchOutcome> {
        Ok(output)
    }
}

/// Blocking fetch implementation (runs on a worker thread; the async gix `receive` is awaited there).
fn do_fetch(repo: Arc<gix::ThreadSafeRepository>, name: String) -> Result<RemoteFetchOutcome> {
    let r = thread_local(&repo);
    let remote = r
        .find_remote(name.as_str())
        .map_err(|e| err("find_remote", e))?;
    let conn = remote
        .connect(gix::remote::Direction::Fetch)
        .map_err(|e| err("connect", e))?;
    let interrupt = AtomicBool::new(false);
    let prepare = conn
        .prepare_fetch(
            gix::progress::Discard,
            gix::remote::ref_map::Options::default(),
        )
        .map_err(|e| err("prepare_fetch", e))?;
    let outcome = prepare
        .receive(gix::progress::Discard, &interrupt)
        .map_err(|e| err("receive", e))?;
    let updated_refs = outcome
        .ref_map
        .mappings
        .iter()
        .filter_map(|m| m.local.as_ref().map(|n| n.to_string()))
        .collect();
    Ok(RemoteFetchOutcome { updated_refs })
}

#[napi]
impl Remote {
    /// Return the URL for `direction` (`"fetch"` or `"push"`), applying rewrite rules.
    #[napi]
    pub fn get_url(&self, direction: Option<String>) -> Result<Option<String>> {
        let r = thread_local(&self.repo);
        let name = match &self.name {
            Some(n) => n.as_str(),
            None => {
                return Ok(self.url.clone());
            }
        };
        let remote = r.find_remote(name).map_err(|e| err("find_remote", e))?;
        let dir = match direction.as_deref() {
            Some("push") => gix::remote::Direction::Push,
            _ => gix::remote::Direction::Fetch,
        };
        Ok(remote.url(dir).map(|u| u.to_string()))
    }

    /// Return all URLs for `direction` (`"fetch"` or `"push"`), applying rewrite rules.
    #[napi]
    pub fn urls(&self, direction: Option<String>) -> Result<Vec<String>> {
        let r = thread_local(&self.repo);
        let name = match &self.name {
            Some(n) => n.as_str(),
            None => {
                return Ok(self.url.clone().into_iter().collect());
            }
        };
        let remote = r.find_remote(name).map_err(|e| err("find_remote", e))?;
        let dir = match direction.as_deref() {
            Some("push") => gix::remote::Direction::Push,
            _ => gix::remote::Direction::Fetch,
        };
        Ok(remote.urls(dir).map(|u| u.to_string()).collect())
    }

    /// Return all refspecs for `direction` (`"fetch"` or `"push"`).
    #[napi]
    pub fn refspecs(&self, direction: Option<String>) -> Result<Vec<String>> {
        let r = thread_local(&self.repo);
        let name = match &self.name {
            Some(n) => n.as_str(),
            None => return Ok(Vec::new()),
        };
        let remote = r.find_remote(name).map_err(|e| err("find_remote", e))?;
        let dir = match direction.as_deref() {
            Some("push") => gix::remote::Direction::Push,
            _ => gix::remote::Direction::Fetch,
        };
        Ok(remote
            .refspecs(dir)
            .iter()
            .map(|s| s.to_ref().to_bstring().to_string())
            .collect())
    }

    /// Fetch from this remote. Resolves a `Promise<RemoteFetchOutcome>`.
    #[napi(ts_return_type = "Promise<RemoteFetchOutcome>")]
    pub fn fetch(&self) -> Result<AsyncTask<FetchTask>> {
        let repo = Arc::clone(&self.repo);
        let name = self.name.clone().ok_or_else(|| {
            napi::Error::from_reason("remote has no name; cannot fetch by name".to_string())
        })?;
        Ok(AsyncTask::new(FetchTask { repo, name }))
    }

    /// Persist this remote to the repository's local config file.
    #[napi]
    pub fn save(&self) -> Result<()> {
        let mut r = thread_local(&self.repo);
        let name = self
            .name
            .as_deref()
            .ok_or_else(|| err("save", "remote has no name"))?;
        let remote = r.find_remote(name).map_err(|e| err("find_remote", e))?;
        let path = r
            .config_path(gix::config::Source::Local)
            .map_err(|e| err("config_path", e))?;
        let mut config_file = r
            .config_file_mut(path)
            .map_err(|e| err("config_file_mut", e))?;
        remote.save_to(&mut config_file).map_err(|e| err("save_to", e))?;
        config_file.commit().map_err(|e| err("commit", e))?;
        r.reload().map_err(|e| err("reload", e))?;
        Ok(())
    }

    /// Save this remote under a new `name` in the local config file.
    #[napi]
    pub fn save_as(&mut self, new_name: String) -> Result<()> {
        let mut r = thread_local(&self.repo);
        let path = r
            .config_path(gix::config::Source::Local)
            .map_err(|e| err("config_path", e))?;
        let mut config_file = r
            .config_file_mut(path)
            .map_err(|e| err("config_file_mut", e))?;

        let mut remote = match self.name.as_deref() {
            Some(n) => r.find_remote(n).map_err(|e| err("find_remote", e))?,
            None => {
                let u = self.url.as_deref().ok_or_else(|| err("save_as", "remote has no url"))?;
                r.remote_at(u).map_err(|e| err("remote_at", e))?
            }
        };
        remote
            .save_as_to(new_name.as_str(), &mut config_file)
            .map_err(|e| err("save_as_to", e))?;
        config_file.commit().map_err(|e| err("commit", e))?;
        r.reload().map_err(|e| err("reload", e))?;
        self.name = Some(new_name);
        Ok(())
    }
}

#[allow(dead_code)]
fn _assert_send() {
    fn is_send<T: Send>() {}
    is_send::<Remote>();
    is_send::<RemoteFetchOutcome>();
    is_send::<Repository>();
}
