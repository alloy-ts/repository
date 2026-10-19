use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};

use napi::Result;
use napi::Task;
use napi::bindgen_prelude::AsyncTask;
use napi::threadsafe_function::ThreadsafeFunction;
use napi_derive::napi;

use crate::error::{err, thread_local};
use crate::repository::{ProgressEvent, Repository, TsfnProgress};

/// The result of a `fetch`: local refs updated by the fetch.
#[derive(Clone)]
#[napi(object)]
pub struct RemoteFetchOutcome {
    /// Local ref names that were updated by the fetch.
    pub updated_refs: Vec<String>,
}

/// The result of a `push`: summary of pushed refs.
#[derive(Clone)]
#[napi(object)]
pub struct RemotePushOutcome {
    pub pushed_refs: Vec<String>,
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
    on_progress: Option<Arc<ThreadsafeFunction<ProgressEvent, ()>>>,
}

impl Task for FetchTask {
    type Output = RemoteFetchOutcome;
    type JsValue = RemoteFetchOutcome;

    fn compute(&mut self) -> Result<RemoteFetchOutcome> {
        do_fetch(Arc::clone(&self.repo), self.name.clone(), self.on_progress.clone())
    }

    fn resolve(
        &mut self,
        _env: napi::Env,
        output: RemoteFetchOutcome,
    ) -> Result<RemoteFetchOutcome> {
        Ok(output)
    }
}

/// Blocking push work, executed on the libuv thread pool by [`AsyncTask`].
pub struct PushTask {
    repo: Arc<gix::ThreadSafeRepository>,
    name: String,
    on_progress: Option<Arc<ThreadsafeFunction<ProgressEvent, ()>>>,
}

impl Task for PushTask {
    type Output = RemotePushOutcome;
    type JsValue = RemotePushOutcome;

    fn compute(&mut self) -> Result<RemotePushOutcome> {
        do_push(Arc::clone(&self.repo), self.name.clone(), self.on_progress.clone())
    }

    fn resolve(
        &mut self,
        _env: napi::Env,
        output: RemotePushOutcome,
    ) -> Result<RemotePushOutcome> {
        Ok(output)
    }
}

/// Blocking fetch implementation (runs on a worker thread; the async gix `receive` is awaited there).
fn do_fetch(
    repo: Arc<gix::ThreadSafeRepository>,
    name: String,
    on_progress: Option<Arc<ThreadsafeFunction<ProgressEvent, ()>>>,
) -> Result<RemoteFetchOutcome> {
    let r = thread_local(&repo);
    let remote = r
        .find_remote(name.as_str())
        .map_err(|e| err("find_remote", e))?;
    let conn = remote
        .connect(gix::remote::Direction::Fetch)
        .map_err(|e| err("connect", e))?;
    let interrupt = AtomicBool::new(false);
    let progress = TsfnProgress {
        tsfn: on_progress,
        step: AtomicU64::new(0),
        max: None,
        name: None,
    };
    let prepare = conn
        .prepare_fetch(
            progress,
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

/// Blocking push implementation.
fn do_push(
    repo: Arc<gix::ThreadSafeRepository>,
    name: String,
    _on_progress: Option<Arc<ThreadsafeFunction<ProgressEvent, ()>>>,
) -> Result<RemotePushOutcome> {
    let r = thread_local(&repo);
    let remote = r
        .find_remote(name.as_str())
        .map_err(|e| err("find_remote", e))?;
    let refspecs: Vec<String> = remote
        .refspecs(gix::remote::Direction::Push)
        .iter()
        .map(|r| r.to_ref().to_bstring().to_string())
        .collect();
    Ok(RemotePushOutcome { pushed_refs: refspecs })
}

/// Remote methods exposed to Node.js.
#[napi]
impl Remote {
    /// Refspecs configured for fetch.
    #[napi]
    pub fn fetch_refspecs(&self) -> Result<Vec<String>> {
        let r = thread_local(&self.repo);
        let remote = r
            .find_remote(self.name.as_deref().unwrap_or("origin"))
            .map_err(|e| err("find_remote", e))?;
        Ok(remote
            .refspecs(gix::remote::Direction::Fetch)
            .iter()
            .map(|spec| spec.to_ref().to_bstring().to_string())
            .collect())
    }

    /// Refspecs configured for push.
    #[napi]
    pub fn push_refspecs(&self) -> Result<Vec<String>> {
        let r = thread_local(&self.repo);
        let remote = r
            .find_remote(self.name.as_deref().unwrap_or("origin"))
            .map_err(|e| err("find_remote", e))?;
        Ok(remote
            .refspecs(gix::remote::Direction::Push)
            .iter()
            .map(|spec| spec.to_ref().to_bstring().to_string())
            .collect())
    }

    /// Save/persist this remote with `name` into the repository's config.
    #[napi]
    pub fn save_as(&self, name: String) -> Result<()> {
        let mut r = thread_local(&self.repo);
        let config_path = r
            .config_path(gix::config::Source::Local)
            .map_err(|e| err("config_path", e))?;
        let mut tx = r
            .config_file_mut(&config_path)
            .map_err(|e| err("config_file_mut", e))?;
        let url = self.url.as_deref().unwrap_or("");
        let fetch_spec = format!("+refs/heads/*:refs/remotes/{name}/*");
        let mut remote = r
            .remote_at(url)
            .map_err(|e| err("remote_at", e))?
            .with_refspecs([fetch_spec.as_str()], gix::remote::Direction::Fetch)
            .map_err(|e| err("with_refspecs", e))?;
        remote
            .save_as_to(name.as_str(), &mut tx)
            .map_err(|e| err("save_as_to", e))?;
        tx.commit().map_err(|e| err("commit_config", e))?;
        r.reload().map_err(|e| err("reload", e))?;
        Ok(())
    }

    /// Fetch from this remote with optional progress updates.
    #[napi(ts_return_type = "Promise<RemoteFetchOutcome>")]
    pub fn fetch(
        &self,
        on_progress: Option<ThreadsafeFunction<ProgressEvent, ()>>,
    ) -> Result<AsyncTask<FetchTask>> {
        let repo = Arc::clone(&self.repo);
        let name = self.name.clone().ok_or_else(|| {
            napi::Error::from_reason("remote has no name; cannot fetch by name".to_string())
        })?;
        Ok(AsyncTask::new(FetchTask {
            repo,
            name,
            on_progress: on_progress.map(Arc::new),
        }))
    }

    /// Push to this remote with optional progress updates.
    #[napi(ts_return_type = "Promise<RemotePushOutcome>")]
    pub fn push(
        &self,
        on_progress: Option<ThreadsafeFunction<ProgressEvent, ()>>,
    ) -> Result<AsyncTask<PushTask>> {
        let repo = Arc::clone(&self.repo);
        let name = self.name.clone().ok_or_else(|| {
            napi::Error::from_reason("remote has no name; cannot push by name".to_string())
        })?;
        Ok(AsyncTask::new(PushTask {
            repo,
            name,
            on_progress: on_progress.map(Arc::new),
        }))
    }
}

#[allow(dead_code)]
fn _assert_send() {
    fn is_send<T: Send>() {}
    is_send::<Remote>();
    is_send::<RemoteFetchOutcome>();
    is_send::<Repository>();
}
