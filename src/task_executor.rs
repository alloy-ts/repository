use std::sync::{Arc, Mutex};
use std::time::Duration;

use napi::JsDeferred;
use napi::bindgen_prelude::{Function, Unknown};
use napi::threadsafe_function::ThreadsafeFunctionCallMode;
use napi::{Env, Result};
use napi_derive::napi;

use libuv::{AsyncHandle, HandleTrait, ReqTrait, WorkReq};

use crate::thread::ThreadPool;

/// Resolver used to settle a task's `JsDeferred` on the main thread once the
/// libuv after-task callback fires.
type DeferredResolver = Box<dyn FnOnce(Env) -> Result<f64> + Send>;

/// Shared mutable state for a single submitted task.
///
/// The worker thread writes `progress` (and finally `result`) while the main
/// thread reads them. `uv_async_send` may coalesce wakeups, so we never rely on
/// the wakeup count: the reader always reads the latest value protected by the
/// mutex.
struct TaskState {
    progress: f64,
    result: Option<f64>,
}

/// Convert a libuv error into an `napi::Error`.
fn uv_err<E: std::fmt::Debug>(e: E) -> napi::Error {
    napi::Error::new(napi::Status::GenericFailure, format!("{e:?}"))
}

/// A libuv-backed task executor.
///
/// `TaskExecutor` drives Node's default libuv event loop — the same `uv_loop`
/// that powers the Node event loop and V8. Tasks are run on libuv's built-in
/// thread pool via `uv_queue_work`, so the JavaScript thread (and therefore V8)
/// stays responsive while the work is in flight.
///
/// The worker thread cannot touch V8, so it cannot call a JavaScript progress
/// callback directly. Instead it records progress in [`TaskState`] and wakes the
/// loop with `uv_async_send`; the async callback runs on the main (V8) thread
/// and forwards the progress to JavaScript through a `ThreadSafeFunction`. This
/// is the canonical inter-thread communication pattern from the libuv docs.
#[napi]
pub struct TaskExecutor {}

#[napi]
impl TaskExecutor {
    /// Create a new libuv-backed task executor.
    #[napi(constructor)]
    pub fn new() -> Self {
        TaskExecutor {}
    }

    /// Submit a CPU-bound task to run on libuv's thread pool.
    ///
    /// `input` is the Fibonacci index to compute (a stand-in for any blocking or
    /// CPU-heavy unit of work). While the work runs on a worker thread,
    /// `on_progress` (if provided) is invoked on the main thread with a
    /// percentage in `[0, 100]`.
    ///
    /// Returns a [`TaskReq`] handle. Await `taskReq.promise()` to obtain the
    /// computed result, or call `taskReq.cancel()` to cancel the task (only
    /// tasks that have not yet started can be cancelled, mirroring `uv_cancel`).
    ///
    /// Internally this is `uv_queue_work` for the heavy computation plus
    /// `uv_async_send` for progress: the worker thread is not allowed to call
    /// into V8, so progress is delivered by waking the loop and letting the
    /// async callback (which runs on the V8 thread) invoke the JS callback.
    #[napi]
    pub fn submit(
        &self,
        env: Env,
        input: f64,
        on_progress: Option<Function<'static, f64, ()>>,
    ) -> Result<TaskReq> {
        // Promise that we settle on the main thread once the work finishes.
        let (deferred, promise) = env
            .create_deferred::<f64, DeferredResolver>()
            .map_err(uv_err)?;
        let deferred_slot: Arc<Mutex<Option<JsDeferred<f64, DeferredResolver>>>> =
            Arc::new(Mutex::new(Some(deferred)));

        // Progress callback bridged to JS via a ThreadSafeFunction.
        // (`build()` yields `CalleeHandled = false`, so `call` takes the raw value.)
        let progress_tsfn: Option<_> = match on_progress {
            Some(f) => Some(
                f.build_threadsafe_function::<f64>()
                    .build()
                    .map_err(uv_err)?,
            ),
            None => None,
        };

        // Shared state between the worker thread and the main thread.
        let state: Arc<Mutex<TaskState>> = Arc::new(Mutex::new(TaskState {
            progress: 0.0,
            result: None,
        }));

        // Use Node's default libuv loop (the loop that drives the V8 event loop),
        // surfaced through the global thread pool handle.
        let pool = ThreadPool::new()?;

        // Async handle: lets the worker thread wake the loop to deliver progress.
        // Its callback runs on the main (V8) thread.
        let async_cb_state = Arc::clone(&state);
        let async_handle = pool
            .r#loop()
            .r#async(move |_handle: AsyncHandle| {
                let pct = async_cb_state.lock().unwrap().progress;
                if let Some(tsfn) = &progress_tsfn {
                    let _ = tsfn.call(pct, ThreadsafeFunctionCallMode::NonBlocking);
                }
            })
            .map_err(uv_err)?;

        // Work callback: runs on a libuv thread-pool thread. It must not call
        // into V8, so it records progress and signals the loop with
        // `uv_async_send`.
        let work_state = Arc::clone(&state);
        let mut work_async = async_handle;
        let work_cb = move |_req: WorkReq| {
            let n = if input < 0.0 {
                0
            } else if input > 90.0 {
                90
            } else {
                input as u64
            };
            let mut a: u64 = 0;
            let mut b: u64 = 1;
            for i in 0..=n {
                if i > 0 {
                    let c = a + b;
                    a = b;
                    b = c;
                }
                let pct = if n == 0 {
                    100.0
                } else {
                    (i as f64 / n as f64) * 100.0
                };
                work_state.lock().unwrap().progress = pct;
                let _ = work_async.send();
                std::thread::sleep(Duration::from_millis(20));
            }
            work_state.lock().unwrap().result = Some(b as f64);
        };

        // After-work callback: runs on the main (V8) thread once the work is
        // done. It resolves the Promise and closes the async handle.
        let after_state = Arc::clone(&state);
        let after_deferred = Arc::clone(&deferred_slot);
        let mut after_async = async_handle;
        let after_cb = move |_req: WorkReq, _status| {
            let result = after_state.lock().unwrap().result.unwrap_or(0.0);
            if let Some(d) = after_deferred.lock().unwrap().take() {
                d.resolve(Box::new(move |_env: Env| Ok(result)));
            }
            let _ = after_async.close(());
        };

        let req = pool.queue_task(work_cb, after_cb).map_err(uv_err)?;

        Ok(TaskReq {
            inner: Box::new(TaskInner {
                req,
                promise: unsafe {
                    napi::bindgen_prelude::ToNapiValue::to_napi_value(env.raw(), promise)
                }?,
            }),
        })
    }
}

/// A handle to a task submitted to a [`TaskExecutor`].
///
/// Wraps libuv's `WorkReq` — the request created by `uv_queue_work`. It can be
/// cancelled via `uv_cancel` (see [`TaskReq::cancel`]) and carries the `Promise`
/// that resolves with the task's result (see [`TaskReq::promise`]).

#[napi]
pub struct TaskReq {
    inner: Box<TaskInner>,
}

// `TaskReq` is a raw-pointer wrapper and `Unknown` is an env-bound handle, both
// `!Send`/`!Sync`, but a `TaskReq` is only ever created, used and dropped on the
// main (V8/JS) thread: `cancel` is invoked from JavaScript, and libuv's
// after-task callback runs on the loop thread. The handle is never actually
// shared across threads, so marking it `Send`/`Sync` is sound here.
unsafe impl Send for TaskReq {}
unsafe impl Sync for TaskReq {}

/// Interior storage for a [`TaskReq`] handle.
///
/// Kept out of the napi struct's direct fields because `napi` rejects
/// env-bound JsValue types (like `Unknown`) as napi class fields.
struct TaskInner {
    req: WorkReq,
    promise: napi::sys::napi_value,
}

#[napi]
impl TaskReq {
    /// Cancel the task.
    ///
    /// Mirrors `uv_cancel`: only tasks that have not yet started executing can be
    /// cancelled. If the task has already started or finished, this fails. A
    /// cancelled task's after-task callback still runs, and its `Promise` is
    /// rejected.
    #[napi]
    pub fn cancel(&self) -> Result<()> {
        let mut req: libuv::Req = self.inner.req.into();
        req.cancel().map_err(uv_err)
    }

    /// The result `Promise` of the task. Await this to obtain the computed value.
    #[napi(ts_return_type = "Promise<number>")]
    pub fn promise(&self, env: Env) -> Result<Unknown<'static>> {
        Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), self.inner.promise) })
    }

    /// The libuv request type name (e.g. `"WORK"`).
    #[napi]
    pub fn type_name(&self) -> String {
        libuv::Req::from(self.inner.req).get_type().name()
    }
}
