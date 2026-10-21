use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use napi::Result;
use napi::Task;
use napi::bindgen_prelude::AsyncTask;
use napi_derive::napi;

use crate::error::{err, thread_local};
use crate::repository::Repository;

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

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
    /// Fetch from this remote. Resolves a `Promise<RemoteFetchOutcome>`.
    #[napi(ts_return_type = "Promise<RemoteFetchOutcome>")]
    pub fn fetch(&self) -> Result<AsyncTask<FetchTask>> {
        let repo = Arc::clone(&self.repo);
        let name = self.name.clone().ok_or_else(|| {
            napi::Error::from_reason("remote has no name; cannot fetch by name".to_string())
        })?;
        Ok(AsyncTask::new(FetchTask { repo, name }))
    }

    /// Return all fetch or push URLs configured for this remote.
    ///
    /// `direction` is `"fetch"` (default) or `"push"`.
    #[napi]
    pub fn urls(&self, direction: Option<String>) -> Result<Vec<String>> {
        let r = thread_local(&self.repo);
        let dir = parse_direction(direction.as_deref());
        let name = self.name.as_deref().unwrap_or("");
        let remote = r.find_remote(name).map_err(|e| err("find_remote", e))?;
        Ok(remote.urls(dir).map(|u| u.to_string()).collect())
    }

    /// Return the push URL configured for this remote, if any.
    #[napi]
    pub fn push_url(&self) -> Result<Option<String>> {
        let r = thread_local(&self.repo);
        let name = self.name.as_deref().unwrap_or("");
        let remote = r.find_remote(name).map_err(|e| err("find_remote", e))?;
        Ok(remote.url(gix::remote::Direction::Push).map(|u| u.to_string()))
    }

    /// Return all refspecs configured for `direction` (`"fetch"` or `"push"`).
    #[napi]
    pub fn refspecs(&self, direction: Option<String>) -> Result<Vec<String>> {
        let r = thread_local(&self.repo);
        let dir = parse_direction(direction.as_deref());
        let name = self.name.as_deref().unwrap_or("");
        let remote = r.find_remote(name).map_err(|e| err("find_remote", e))?;
        Ok(remote
            .refspecs(dir)
            .iter()
            .map(|spec| lossy(&spec.to_ref().to_bstring()))
            .collect())
    }

    /// Return tag fetching strategy as a string: `"all"`, `"none"`, or `"included"`.
    #[napi]
    pub fn fetch_tags(&self) -> Result<String> {
        let r = thread_local(&self.repo);
        let name = self.name.as_deref().unwrap_or("");
        let remote = r.find_remote(name).map_err(|e| err("find_remote", e))?;
        let tags = match remote.fetch_tags() {
            gix::remote::fetch::Tags::All => "all",
            gix::remote::fetch::Tags::None => "none",
            gix::remote::fetch::Tags::Included => "included",
        };
        Ok(tags.to_string())
    }

    /// Set a new fetch URL for this remote and persist it to config.
    #[napi]
    pub fn set_url(&self, url: String) -> Result<()> {
        let mut r = thread_local(&self.repo);
        let name = self.name.as_deref().ok_or_else(|| err("set_url", "anonymous remote cannot be saved"))?;
        let key = format!("remote.{}.url", name);
        let config_path = r.config_path(gix::config::Source::Local).map_err(|e| err("config_path", e))?;
        let mut config_file = r.config_file_mut(config_path).map_err(|e| err("config_file_mut", e))?;
        config_file.set_raw_value(key.as_str(), url.as_str()).map_err(|e| err("set_raw_value", e))?;
        config_file.commit().map_err(|e| err("commit", e))?;
        r.reload().map_err(|e| err("reload", e))?;
        Ok(())
    }

    /// Set an explicit push URL for this remote and persist it to config.
    #[napi]
    pub fn set_push_url(&self, url: String) -> Result<()> {
        let mut r = thread_local(&self.repo);
        let name = self.name.as_deref().ok_or_else(|| err("set_push_url", "anonymous remote cannot be saved"))?;
        let key = format!("remote.{}.pushurl", name);
        let config_path = r.config_path(gix::config::Source::Local).map_err(|e| err("config_path", e))?;
        let mut config_file = r.config_file_mut(config_path).map_err(|e| err("config_file_mut", e))?;
        config_file.set_raw_value(key.as_str(), url.as_str()).map_err(|e| err("set_raw_value", e))?;
        config_file.commit().map_err(|e| err("commit", e))?;
        r.reload().map_err(|e| err("reload", e))?;
        Ok(())
    }

    /// Save or overwrite this remote in the local config file.
    #[napi]
    pub fn save_to_config(&self) -> Result<()> {
        let mut r = thread_local(&self.repo);
        let name = self.name.as_deref().ok_or_else(|| err("save_to_config", "anonymous remote cannot be saved"))?;
        let remote = r.find_remote(name).map_err(|e| err("find_remote", e))?;
        let config_path = r.config_path(gix::config::Source::Local).map_err(|e| err("config_path", e))?;
        let mut config_file = r.config_file_mut(config_path).map_err(|e| err("config_file_mut", e))?;
        remote.save_to(&mut config_file).map_err(|e| err("save_to", e))?;
        config_file.commit().map_err(|e| err("commit", e))?;
        r.reload().map_err(|e| err("reload", e))?;
        Ok(())
    }
}

fn parse_direction(dir: Option<&str>) -> gix::remote::Direction {
    match dir {
        Some("push") => gix::remote::Direction::Push,
        _ => gix::remote::Direction::Fetch,
    }
}

#[allow(dead_code)]
fn _assert_send() {
    fn is_send<T: Send>() {}
    is_send::<Remote>();
    is_send::<RemoteFetchOutcome>();
    is_send::<Repository>();
}
