use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use napi::Task;
use napi::bindgen_prelude::AsyncTask;
use napi::Result;
use napi_derive::napi;

use crate::repository::Repository;

/// Blocking clone work, executed on the libuv thread pool by [`AsyncTask`].
pub struct CloneTask {
    pub(crate) url: String,
    pub(crate) path: String,
    pub(crate) bare: bool,
}

impl Task for CloneTask {
    type Output = Repository;
    type JsValue = Repository;

    fn compute(&mut self) -> Result<Repository> {
        let mut prepare = if self.bare {
            gix::prepare_clone_bare(self.url.clone(), self.path.as_str())
        } else {
            gix::prepare_clone(self.url.clone(), self.path.as_str())
        }
        .map_err(|e| napi::Error::from_reason(format!("prepare_clone: {e}")))?;
        let interrupt = AtomicBool::new(false);
        let (repo, _outcome) = prepare
            .fetch_only(gix::progress::Discard, &interrupt)
            .map_err(|e| napi::Error::from_reason(format!("fetch: {e}")))?;
        Ok(Repository {
            inner: Arc::new(repo.into()),
        })
    }

    fn resolve(&mut self, _env: napi::Env, output: Repository) -> Result<Repository> {
        Ok(output)
    }
}

/// Clone a repository from `url` into `path`.
///
/// Runs the (blocking) network transfer on a worker thread and resolves a `Promise<Repository>`.
#[napi(ts_return_type = "Promise<Repository>")]
pub fn clone(url: String, path: String, bare: Option<bool>) -> AsyncTask<CloneTask> {
    AsyncTask::new(CloneTask {
        url,
        path,
        bare: bare.unwrap_or(false),
    })
}
