use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use futures::executor::{LocalPool as FuturesLocalPool, LocalSpawner as FuturesLocalSpawner};
use futures_executor::{ThreadPool as FuturesThreadPool, ThreadPoolBuilder};
use futures::task::LocalSpawnExt;
use futures_channel::oneshot;
use napi::JsDeferred;
use napi::bindgen_prelude::{Function, JsValue, ToNapiValue, Unknown};
use napi::threadsafe_function::ThreadsafeFunctionCallMode;
use napi::{Env, Result};
use napi_derive::napi;

use libuv::{ReqTrait, WorkReq};

use crate::task::{Task, TaskOptions, TaskSignal};
use crate::thread::ThreadPool;

/// Which scheduler drives `submit`/`spawn` (the *task-based* path). The
/// *future-based* path (`submit_work`/`compute_parallel`) is always
/// available regardless of `kind`.
#[napi]
pub enum SchedulerKind {
    /// Node's default libuv thread pool (`uv_queue_work` + `uv_async_send`).
    Libuv,
    /// The `futures` crate's M:N thread pool (`futures::scheduler::ThreadPool`).
    Futures,
    /// A single-threaded `futures::scheduler::LocalPool` (accepts `!Send` work).
    Local,
}

/// Configuration for constructing a unified [`Scheduler`].
#[napi(object)]
pub struct SchedulerOptions {
    /// Scheduler for the task-based `submit`/`spawn` path. Defaults to
    /// [`SchedulerKind::Libuv`] when `None`.
    pub kind: Option<SchedulerKind>,
    /// Thread count for the `Futures` scheduler (ignored otherwise).
    pub pool_size: Option<u32>,
    /// Thread name prefix for the `Futures` scheduler.
    pub name_prefix: Option<String>,
    /// Thread stack size (bytes) for the `Futures` scheduler.
    pub stack_size: Option<i64>,
}

/// Resolver used to settle a numeric (`f64`) `JsDeferred` on the main thread.
type DeferredResolver = Box<dyn FnOnce(Env) -> Result<f64> + Send>;

/// Resolver for a `Promise<unknown>`-typed deferred.
type PromiseResolver<T> = Box<dyn FnOnce(Env) -> Result<T> + Send>;

/// Shared mutable state for a single libuv-backed submitted task.
struct TaskState {
    progress: f64,
    result: Option<f64>,
}

/// Convert a libuv (or any `Debug`) error into an `napi::Error`.
fn uv_err<E: std::fmt::Debug>(e: E) -> napi::Error {
    napi::Error::new(napi::Status::GenericFailure, format!("{e:?}"))
}

/// How a [`Task`] may be cancelled, depending on the backend that produced it.
enum TaskCancel {
    /// libuv: cancel via `uv_cancel` on the underlying `WorkReq`.
    Libuv(WorkReq),
    /// futures / local: cooperative `cancel` + `started` flags.
    Futures(Arc<AtomicBool>, Arc<AtomicBool>),
}

/// Interior storage for a [`Task`] handle.
struct TaskInner {
    promise: napi::sys::napi_value,
    cancel: TaskCancel,
    type_name: String,
}

/// A handle to a task submitted to any [`Scheduler`].
///
/// Mirrors the previous libuv `TaskReq` and futures `FutureTask` handles: it
/// carries the result `Promise` and supports cooperative cancellation.
///
/// NOTE: this is distinct from the Scheduler-style [`crate::task::Task`] (which
/// runs a `work` with sandboxing, `taskData`, event handlers and
/// `terminate`). That one is reached through [`Scheduler::exec`]; this
/// `TaskHandle` is the lightweight result handle returned by
/// [`Scheduler::submit`].
#[napi]
pub struct TaskHandle {
    inner: Box<TaskInner>,
}

// `TaskHandle` wraps an env-bound `napi_value` and possibly a libuv `WorkReq`
// (both `!Send`/`!Sync`), but a `TaskHandle` is only ever created, used and
// dropped on the JS thread, so marking it `Send`/`Sync` is sound here.
unsafe impl Send for TaskHandle {}
unsafe impl Sync for TaskHandle {}

#[napi]
impl TaskHandle {
    /// Cancel the task.
    ///
    /// For the libuv backend this calls `uv_cancel`; it returns `true` if the
    /// cancel took effect, `false` if the task had already started/completed.
    /// For the futures/local backend cancellation is cooperative and only
    /// succeeds if the task has not yet started.
    #[napi]
    pub fn cancel(&self) -> Result<bool> {
        match &self.inner.cancel {
            TaskCancel::Libuv(req) => {
                let mut r: libuv::Req = libuv::Req::from(*req);
                r.cancel().map(|_| true).or_else(|_| Ok(false))
            }
            TaskCancel::Futures(cancel, started) => {
                if started.load(Ordering::Relaxed) {
                    return Ok(false);
                }
                cancel.store(true, Ordering::Relaxed);
                Ok(true)
            }
        }
    }

    /// The result `Promise` of the task. Await this to obtain the computed value.
    #[napi(ts_return_type = "Promise<number>")]
    pub fn promise(&self, env: Env) -> Result<Unknown<'static>> {
        Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), self.inner.promise) })
    }

    /// A stable type-name for the request (e.g. `"LIBUV_TASK"` / `"FUTURE_TASK"`).
    #[napi]
    pub fn type_name(&self) -> String {
        self.inner.type_name.clone()
    }
}

/// A task queued onto a `Local` scheduler's dedicated driver thread.
type LocalTask = Box<dyn FnOnce(&FuturesLocalSpawner) + Send + 'static>;

/// The single-threaded (`Local`) backend: a dedicated OS thread that drives a
/// `futures::scheduler::LocalPool` loop.
struct LocalDriver {
    spawn_tx: std::sync::mpsc::Sender<LocalTask>,
    stop: Arc<AtomicBool>,
    driver: Option<thread::JoinHandle<()>>,
}

impl LocalDriver {
    fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<LocalTask>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_f = Arc::clone(&stop);

        let driver = thread::spawn(move || {
            let mut pool = FuturesLocalPool::new();
            let spawner = pool.spawner();
            loop {
                while let Ok(task) = rx.try_recv() {
                    task(&spawner);
                }
                pool.run_until_stalled();
                if stop_f.load(Ordering::Relaxed) {
                    while let Ok(task) = rx.try_recv() {
                        task(&spawner);
                    }
                    pool.run_until_stalled();
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
        });

        LocalDriver {
            spawn_tx: tx,
            stop,
            driver: Some(driver),
        }
    }

    /// Submit the Fibonacci stand-in task onto the local pool (task-based path).
    fn submit(
        &self,
        env: Env,
        input: f64,
        on_progress: Option<Function<'static, f64, ()>>,
    ) -> Result<TaskHandle> {
        let (deferred, promise) = env
            .create_deferred::<f64, DeferredResolver>()
            .map_err(uv_err)?;
        let deferred_slot: Arc<Mutex<Option<JsDeferred<f64, DeferredResolver>>>> =
            Arc::new(Mutex::new(Some(deferred)));

        let progress_tsfn: Option<_> = match on_progress {
            Some(f) => Some(
                f.build_threadsafe_function::<f64>()
                    .build()
                    .map_err(uv_err)?,
            ),
            None => None,
        };

        let resolve_slot = Arc::clone(&deferred_slot);
        let resolve_fn = env.create_function_from_closure("__localResolve", move |ctx| {
            let result: f64 = ctx.get(0)?;
            if let Some(d) = resolve_slot.lock().unwrap().take() {
                d.resolve(Box::new(move |_env: Env| Ok(result)));
            }
            Ok(())
        })?;
        let resolve_tsfn = resolve_fn
            .build_threadsafe_function::<f64>()
            .build()
            .map_err(uv_err)?;

        let cancel = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicBool::new(false));
        let cancel_f = Arc::clone(&cancel);
        let started_f = Arc::clone(&started);
        let progress_tsfn_f = progress_tsfn;
        let resolve_tsfn_f = resolve_tsfn;

        let n = if input < 0.0 {
            0
        } else if input > 90.0 {
            90
        } else {
            input as u64
        };

        let fut = async move {
            if cancel_f.load(Ordering::Relaxed) {
                let _ = resolve_tsfn_f.call(0.0, ThreadsafeFunctionCallMode::NonBlocking);
                return;
            }
            started_f.store(true, Ordering::Relaxed);

            let mut a: u64 = 0;
            let mut b: u64 = 1;
            for i in 0..=n {
                if cancel_f.load(Ordering::Relaxed) {
                    break;
                }
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
                if let Some(tsfn) = &progress_tsfn_f {
                    let _ = tsfn.call(pct, ThreadsafeFunctionCallMode::NonBlocking);
                }
                thread::sleep(Duration::from_millis(10));
            }
            let _ = resolve_tsfn_f.call(b as f64, ThreadsafeFunctionCallMode::NonBlocking);
        };

        self.spawn_tx
            .send(Box::new(move |spawner: &FuturesLocalSpawner| {
                let _ = spawner.spawn_local(async move { fut.await });
            }))
            .map_err(|e| {
                napi::Error::new(
                    napi::Status::GenericFailure,
                    format!("failed to enqueue: {e}"),
                )
            })?;

        Ok(TaskHandle {
            inner: Box::new(TaskInner {
                promise: unsafe { ToNapiValue::to_napi_value(env.raw(), promise)? },
                cancel: TaskCancel::Futures(cancel, started),
                type_name: "LOCAL_TASK".to_string(),
            }),
        })
    }

    /// Fire-and-forget a JS callback onto the local pool.
    fn spawn(&self, task: Function<'static, (), ()>) -> Result<()> {
        let tsfn = task
            .build_threadsafe_function::<()>()
            .build()
            .map_err(uv_err)?;
        self.spawn_tx
            .send(Box::new(move |spawner: &FuturesLocalSpawner| {
                let _ = spawner.spawn_local(async move {
                    let _ = tsfn.call((), ThreadsafeFunctionCallMode::NonBlocking);
                });
            }))
            .map_err(|e| {
                napi::Error::new(
                    napi::Status::GenericFailure,
                    format!("failed to enqueue: {e}"),
                )
            })?;
        Ok(())
    }
}

impl Drop for LocalDriver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.driver.take() {
            let _ = handle.join();
        }
    }
}

/// The backend that drives `submit`/`spawn` (the task-based path). The
/// future-based path is always available via the lazily-created `FuturesThreadPool`.
enum Backend {
    Libuv,
    Futures,
    Local(LocalDriver),
}

/// A unified, generic task scheduler exposed to JavaScript via napi.
///
/// One class that wraps the libuv thread pool, the `futures::scheduler::ThreadPool`,
/// and the single-threaded `LocalPool`. The `kind` option selects which scheduler
/// drives the *task-based* `submit`/`spawn` path, while the *future-based* path
/// (`submit_work`, `compute_parallel`) is always available.
#[napi]
pub struct Scheduler {
    backend: Backend,
    /// Lazily-created `futures` pool, used by the `Futures` backend and by the
    /// future-based methods on any backend.
    futures: Mutex<Option<FuturesThreadPool>>,
}

#[napi]
impl Scheduler {
    /// Create a new scheduler. `options.kind` selects the task-based scheduler
    /// (defaults to libuv); the pool-sizing options only apply to the `Futures`
    /// scheduler.
    #[napi(constructor)]
    pub fn new(options: Option<SchedulerOptions>) -> Result<Self> {
        let opts = options.unwrap_or(SchedulerOptions {
            kind: None,
            pool_size: None,
            name_prefix: None,
            stack_size: None,
        });
        let kind = opts.kind.unwrap_or(SchedulerKind::Libuv);

        let backend = match kind {
            SchedulerKind::Libuv => Backend::Libuv,
            SchedulerKind::Futures => Backend::Futures,
            SchedulerKind::Local => Backend::Local(LocalDriver::new()),
        };

        Ok(Scheduler {
            backend,
            futures: Mutex::new(None),
        })
    }

    /// A task-based `submit`: run a CPU-bound unit of work and obtain a [`Task`]
    /// whose `promise()` resolves with the result.
    ///
    /// The work is a stand-in Fibonacci computation; `on_progress` (if provided)
    /// is invoked on the event-loop thread with a percentage in `[0, 100]`.
    /// Cancellation is cooperative (libuv) or pre-start-only (futures/local).
    #[napi]
    pub fn submit(
        &self,
        env: Env,
        input: f64,
        on_progress: Option<Function<'static, f64, ()>>,
    ) -> Result<TaskHandle> {
        match &self.backend {
            Backend::Libuv => Self::submit_libuv(env, input, on_progress),
            Backend::Futures => {
                Self::submit_futures(&self.futures_pool()?, env, input, on_progress)
            }
            Backend::Local(driver) => driver.submit(env, input, on_progress),
        }
    }

    /// Future-based `submit`: run the same CPU-bound work but resolve with the
    /// value directly. The `#[napi] async fn` is wrapped into a JS `Promise` by
    /// napi (driven by the `tokio_rt` runtime), while the work itself runs on
    /// the `futures` thread pool. This is the "future-based on the Rust side"
    /// counterpart to [`Scheduler::submit`].
    ///
    /// Progress reporting is intentionally omitted here: a `Function` argument is
    /// not `Send`, so it cannot be captured into an `#[napi] async fn`'s future
    /// (which napi drives across threads). Use [`Scheduler::submit`] for progress.
    #[napi]
    pub async fn submit_async(&self, input: f64) -> Result<f64> {
        let pool = self.futures_pool()?;
        let (tx, rx) = oneshot::channel::<f64>();
        let n = if input < 0.0 {
            0
        } else if input > 90.0 {
            90
        } else {
            input as u64
        };
        let _ = pool.spawn_ok(async move {
            let mut a: u64 = 0;
            let mut b: u64 = 1;
            for i in 0..=n {
                if i > 0 {
                    let c = a + b;
                    a = b;
                    b = c;
                }
                thread::sleep(Duration::from_millis(10));
            }
            let _ = tx.send(b as f64);
        });
        rx.await.map_err(|_| {
            napi::Error::new(
                napi::Status::GenericFailure,
                "task was cancelled before completion",
            )
        })
    }

    /// Fire-and-forget: spawn a JS `task` callback onto the scheduler.
    ///
    /// The callback is dispatched to the event-loop thread via a
    /// `ThreadSafeFunction`; the task thread only triggers the call.
    #[napi]
    pub fn spawn(&self, task: Function<'static, (), ()>) -> Result<()> {
        match &self.backend {
            Backend::Libuv => Self::spawn_libuv(task),
            Backend::Futures => {
                let pool = self.futures_pool()?;
                let tsfn = task
                    .build_threadsafe_function::<()>()
                    .build()
                    .map_err(uv_err)?;
                pool.spawn_ok(async move {
                    let _ = tsfn.call((), ThreadsafeFunctionCallMode::NonBlocking);
                });
                Ok(())
            }
            Backend::Local(driver) => driver.spawn(task),
        }
    }

    /// Future-based execution: run a runnable string (a JS work) and resolve
    /// with its return value. Always available (uses the `futures` runtime).
    #[napi(ts_return_type = "Promise<unknown>")]
    pub fn submit_work(&self, env: Env, work: String) -> Result<Unknown<'static>> {
        let pool = self.futures_pool()?;
        let (deferred, promise) = env
            .create_deferred::<Unknown<'static>, PromiseResolver<Unknown<'static>>>()
            .map_err(uv_err)?;
        let slot: Arc<
            Mutex<Option<JsDeferred<Unknown<'static>, PromiseResolver<Unknown<'static>>>>>,
        > = Arc::new(Mutex::new(Some(deferred)));

        let code = format!("(function () {{ {work} }})");
        let resolve_slot = Arc::clone(&slot);
        let resolve_fn = env.create_function_from_closure("__workResolve", move |ctx| {
            let f: Function<(), Unknown<'static>> = ctx.env.run_script(&code)?;
            let result = f.call(())?;
            // `Unknown` is `!Send`; pass the raw handle as a `usize` and rebuild
            // it on the JS thread.
            let raw = result.raw() as usize;
            if let Some(d) = resolve_slot.lock().unwrap().take() {
                d.resolve(Box::new(move |_env: Env| {
                    let unknown = unsafe {
                        Unknown::from_raw_unchecked(_env.raw(), raw as napi::sys::napi_value)
                    };
                    Ok(unknown)
                }));
            }
            Ok(())
        })?;
        let resolve_tsfn = resolve_fn
            .build_threadsafe_function::<()>()
            .build()
            .map_err(uv_err)?;

        pool.spawn_ok(async move {
            let _ = resolve_tsfn.call((), ThreadsafeFunctionCallMode::NonBlocking);
        });

        let promise_raw = unsafe { ToNapiValue::to_napi_value(env.raw(), promise)? };
        Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), promise_raw) })
    }

    /// Future-based execution: run two Rust async computations concurrently with
    /// `futures::join!` and resolve with their results as a `[number, number]`
    /// tuple. Always available (uses the `futures` runtime).
    #[napi(ts_return_type = "Promise<[number, number]>")]
    pub fn compute_parallel(&self, env: Env, a: f64, b: f64) -> Result<Unknown<'static>> {
        let pool = self.futures_pool()?;
        let fib = |n: u64| async move {
            let mut x: u64 = 0;
            let mut y: u64 = 1;
            for _ in 0..n {
                let c = x + y;
                x = y;
                y = c;
            }
            y as f64
        };

        let (deferred, promise) = env
            .create_deferred::<Unknown<'static>, PromiseResolver<Unknown<'static>>>()
            .map_err(uv_err)?;
        let slot: Arc<
            Mutex<Option<JsDeferred<Unknown<'static>, PromiseResolver<Unknown<'static>>>>>,
        > = Arc::new(Mutex::new(Some(deferred)));

        let resolve_slot = Arc::clone(&slot);
        let resolve_fn = env.create_function_from_closure("__joinResolve", move |ctx| {
            let v: Vec<f64> = ctx.get(0)?;
            if let Some(d) = resolve_slot.lock().unwrap().take() {
                d.resolve(Box::new(move |env: Env| {
                    let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), v)? };
                    Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), raw) })
                }));
            }
            Ok(())
        })?;
        let resolve_tsfn = resolve_fn
            .build_threadsafe_function::<Vec<f64>>()
            .build()
            .map_err(uv_err)?;

        let na = if a < 0.0 {
            0
        } else if a > 90.0 {
            90
        } else {
            a as u64
        };
        let nb = if b < 0.0 {
            0
        } else if b > 90.0 {
            90
        } else {
            b as u64
        };
        pool.spawn_ok(async move {
            let (ra, rb) = futures::join!(fib(na), fib(nb));
            let _ = resolve_tsfn.call(vec![ra, rb], ThreadsafeFunctionCallMode::NonBlocking);
        });

        let promise_raw = unsafe { ToNapiValue::to_napi_value(env.raw(), promise)? };
        Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), promise_raw) })
    }

    /// Run a Node-Scheduler-compatible [`Task`] (a `work` with sandboxing,
    /// `taskData`, event handlers and `terminate`) through this scheduler.
    ///
    /// This folds the `task.rs` `Task`/`Programme` machinery into the unified
    /// scheduler's surface: blocking setup (sandbox worktree creation,
    /// object-database reads, network fetches) runs on a libuv thread-pool
    /// thread and the work is evaluated on the JS thread in the
    /// after-work callback, exactly as `new Task(options)` would.
    #[napi]
    pub fn run(&self, env: Env, options: TaskOptions) -> Result<Task> {
        Task::new(env, options)
    }

    /// Create a [`TaskSignal`] (libuv `uv_async_t`) wakeup handle bound to this
    /// scheduler's loop.
    ///
    /// Calling `send()` on the returned handle from *any* thread wakes the Node
    /// event loop and runs `cb` on the loop thread — the same task→loop
    /// primitive the scheduler uses internally to deliver progress and results
    /// from `uv_queue_work` jobs back to JavaScript.
    #[napi]
    pub fn async_waker(&self, _env: Env, cb: Function<'static, (), ()>) -> Result<TaskSignal> {
        TaskSignal::new(cb)
    }

    /// Return a handle to the `futures` pool, creating it on first use.
    fn futures_pool(&self) -> Result<FuturesThreadPool> {
        let mut g = self.futures.lock().unwrap();
        if g.is_none() {
            let mut builder = ThreadPoolBuilder::new();
            // Reuse size hints from a potential Futures construction is unnecessary
            // here; the future-based methods use a default-sized pool.
            *g = Some(builder.create().map_err(uv_err)?);
        }
        Ok(g.as_ref().unwrap().clone())
    }

    // --- libuv task-based helpers ---------------------------------------------

    fn submit_libuv(
        env: Env,
        input: f64,
        on_progress: Option<Function<'static, f64, ()>>,
    ) -> Result<TaskHandle> {
        let (deferred, promise) = env
            .create_deferred::<f64, DeferredResolver>()
            .map_err(uv_err)?;
        let deferred_slot: Arc<Mutex<Option<JsDeferred<f64, DeferredResolver>>>> =
            Arc::new(Mutex::new(Some(deferred)));

        let progress_tsfn: Option<_> = match on_progress {
            Some(f) => Some(
                f.build_threadsafe_function::<f64>()
                    .build()
                    .map_err(uv_err)?,
            ),
            None => None,
        };

        let state: Arc<Mutex<TaskState>> = Arc::new(Mutex::new(TaskState {
            progress: 0.0,
            result: None,
        }));

        let pool = ThreadPool::new().map_err(uv_err)?;

        let async_cb_state = Arc::clone(&state);
        let async_handle = TaskSignal::wake_on(pool.r#loop(), move || {
            let pct = async_cb_state.lock().unwrap().progress;
            if let Some(tsfn) = &progress_tsfn {
                let _ = tsfn.call(pct, ThreadsafeFunctionCallMode::NonBlocking);
            }
        })?;

        let work_state = Arc::clone(&state);
        let work_async = async_handle;
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

        let after_state = Arc::clone(&state);
        let after_deferred = Arc::clone(&deferred_slot);
        let after_async = async_handle;
        let after_cb = move |_req: WorkReq, _status| {
            let result = after_state.lock().unwrap().result.unwrap_or(0.0);
            if let Some(d) = after_deferred.lock().unwrap().take() {
                d.resolve(Box::new(move |_env: Env| Ok(result)));
            }
            let _ = after_async.close();
        };

        let req = pool.run(work_cb, after_cb).map_err(uv_err)?;
        let type_name = libuv::Req::from(req).get_type().name().to_string();

        Ok(TaskHandle {
            inner: Box::new(TaskInner {
                promise: unsafe { ToNapiValue::to_napi_value(env.raw(), promise)? },
                cancel: TaskCancel::Libuv(req),
                type_name,
            }),
        })
    }

    fn spawn_libuv(task: Function<'static, (), ()>) -> Result<()> {
        let tsfn = task
            .build_threadsafe_function::<()>()
            .build()
            .map_err(uv_err)?;
        let pool = ThreadPool::new().map_err(uv_err)?;
        let work = move |_req: WorkReq| {
            let _ = tsfn.call((), ThreadsafeFunctionCallMode::NonBlocking);
        };
        let after = move |_req: WorkReq, _status| {};
        pool.run(work, after).map_err(uv_err)?;
        Ok(())
    }

    // --- futures task-based helper (used by the `Futures` backend) -------------

    fn submit_futures(
        pool: &FuturesThreadPool,
        env: Env,
        input: f64,
        on_progress: Option<Function<'static, f64, ()>>,
    ) -> Result<TaskHandle> {
        let (deferred, promise) = env
            .create_deferred::<f64, DeferredResolver>()
            .map_err(uv_err)?;
        let deferred_slot: Arc<Mutex<Option<JsDeferred<f64, DeferredResolver>>>> =
            Arc::new(Mutex::new(Some(deferred)));

        let progress_tsfn: Option<_> = match on_progress {
            Some(f) => Some(
                f.build_threadsafe_function::<f64>()
                    .build()
                    .map_err(uv_err)?,
            ),
            None => None,
        };

        let resolve_slot = Arc::clone(&deferred_slot);
        let resolve_fn = env.create_function_from_closure("__futureResolve", move |ctx| {
            let result: f64 = ctx.get(0)?;
            if let Some(d) = resolve_slot.lock().unwrap().take() {
                d.resolve(Box::new(move |_env: Env| Ok(result)));
            }
            Ok(())
        })?;
        let resolve_tsfn = resolve_fn
            .build_threadsafe_function::<f64>()
            .build()
            .map_err(uv_err)?;

        let cancel = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicBool::new(false));
        let cancel_f = Arc::clone(&cancel);
        let started_f = Arc::clone(&started);
        let progress_tsfn_f = progress_tsfn;
        let resolve_tsfn_f = resolve_tsfn;

        let n = if input < 0.0 {
            0
        } else if input > 90.0 {
            90
        } else {
            input as u64
        };

        let fut = async move {
            if cancel_f.load(Ordering::Relaxed) {
                let _ = resolve_tsfn_f.call(0.0, ThreadsafeFunctionCallMode::NonBlocking);
                return;
            }
            started_f.store(true, Ordering::Relaxed);

            let mut a: u64 = 0;
            let mut b: u64 = 1;
            for i in 0..=n {
                if cancel_f.load(Ordering::Relaxed) {
                    break;
                }
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
                if let Some(tsfn) = &progress_tsfn_f {
                    let _ = tsfn.call(pct, ThreadsafeFunctionCallMode::NonBlocking);
                }
                thread::sleep(Duration::from_millis(10));
            }
            let _ = resolve_tsfn_f.call(b as f64, ThreadsafeFunctionCallMode::NonBlocking);
        };

        pool.spawn_ok(fut);

        Ok(TaskHandle {
            inner: Box::new(TaskInner {
                promise: unsafe { ToNapiValue::to_napi_value(env.raw(), promise)? },
                cancel: TaskCancel::Futures(cancel, started),
                type_name: "FUTURE_TASK".to_string(),
            }),
        })
    }
}

/// Re-export the low-level `libuv` bindings under a feature, mirroring the `sys`
/// module pattern that the underlying `libuv` crate itself uses.
#[cfg(feature = "sys")]
#[allow(unused_imports)]
pub mod sys {
    pub use libuv::*;
}
