use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gix::bstr::ByteSlice;
use napi::Task;
use napi::bindgen_prelude::AsyncTask;
use napi::Result;
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

    fn resolve(&mut self, _env: napi::Env, output: RemoteFetchOutcome) -> Result<RemoteFetchOutcome> {
        Ok(output)
    }
}

/// Blocking fetch implementation (runs on a worker thread).
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
        .prepare_fetch(gix::progress::Discard, gix::remote::ref_map::Options::default())
        .map_err(|e| err("prepare_fetch", e))?;
    let outcome = prepare
        .receive(gix::progress::Discard, &interrupt)
        .map_err(|e| err("receive", e))?;
    let updated_refs = outcome
        .ref_map
        .mappings
        .iter()
        .filter_map(|m| m.local.as_ref().map(|n| lossy(n.as_bstr())))
        .collect();
    Ok(RemoteFetchOutcome { updated_refs })
}

fn lossy(b: &gix::bstr::BStr) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Fetch from this remote. Resolves a `Promise<RemoteFetchOutcome>`.
#[napi]
impl Remote {
    #[napi(ts_return_type = "Promise<RemoteFetchOutcome>")]
    pub fn fetch(&self) -> Result<AsyncTask<FetchTask>> {
        let repo = Arc::clone(&self.repo);
        let name = self.name.clone().ok_or_else(|| {
            napi::Error::from_reason("remote has no name; cannot fetch by name".to_string())
        })?;
        Ok(AsyncTask::new(FetchTask { repo, name }))
    }
}

#[allow(dead_code)]
fn _assert_send() {
    fn is_send<T: Send>() {}
    is_send::<Remote>();
    is_send::<RemoteFetchOutcome>();
    is_send::<Repository>();
}
