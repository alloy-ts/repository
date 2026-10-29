use std::sync::{Arc, Mutex};
use std::thread::{self as std_thread, ThreadId as StdThreadId};
use std::time::Duration;

use napi::bindgen_prelude::{Error, Function, Result, Status};
use napi::threadsafe_function::ThreadsafeFunctionCallMode;
use napi_derive::napi;

use libuv::{AfterWorkCB, Loop, WorkCB, WorkReq};

/// A handle to a thread.
///
/// Exposed to JS as the `Thread` class. Wraps [`std::thread::Thread`].
#[napi]
pub struct Thread {
    inner: std_thread::Thread,
}

#[napi]
impl Thread {
    /// Returns a handle to the currently running thread.
    #[napi]
    pub fn current() -> Thread {
        Thread {
            inner: std_thread::current(),
        }
    }

    /// Returns a unique identifier for this thread (as a JS number).
    #[napi(getter)]
    pub fn id(&self) -> f64 {
        thread_id_to_u64(self.inner.id()) as f64
    }

    /// Returns the name of the thread, if one was set.
    #[napi(getter)]
    pub fn name(&self) -> Option<String> {
        self.inner.name().map(String::from)
    }

    /// Atomically makes the handle's token available if it is not already.
    ///
    /// Used together with [`Thread::park`] / [`Thread::park_timeout`].
    #[napi]
    pub fn unpark(&self) {
        self.inner.unpark()
    }

    /// Puts the current thread to sleep for at least `millis` milliseconds.
    #[napi]
    pub fn sleep(millis: f64) {
        std_thread::sleep(Duration::from_secs_f64(millis / 1000.0));
    }

    /// Cooperatively yields the current thread's timeslice to the scheduler.
    #[napi]
    pub fn yield_now() {
        std_thread::yield_now();
    }

    /// Blocks the current thread until its token is made available by
    /// [`Thread::unpark`].
    #[napi]
    pub fn park() {
        std_thread::park();
    }

    /// Like [`Thread::park`], but blocks for at most `millis` milliseconds.
    #[napi]
    pub fn park_timeout(millis: f64) {
        std_thread::park_timeout(Duration::from_secs_f64(millis / 1000.0));
    }
}

/// Thread factory, mirroring [`std::thread::Builder`].
///
/// Configure a thread's `name` / `stack_size`, then call [`Builder::spawn`].
#[napi]
#[derive(Clone)]
pub struct Builder {
    name: Option<String>,
    stack_size: Option<usize>,
}

#[napi]
impl Builder {
    /// Generates the base configuration for spawning a thread.
    #[napi]
    pub fn new() -> Builder {
        Builder {
            name: None,
            stack_size: None,
        }
    }

    /// Names the thread-to-be (used for identification in panic messages).
    #[napi]
    pub fn name(&mut self, name: String) -> Builder {
        self.name = Some(name);
        self.clone()
    }

    /// Sets the size of the stack (in bytes) for the new thread.
    #[napi]
    pub fn stack_size(&mut self, size: i64) -> Builder {
        if size > 0 {
            self.stack_size = Some(size as usize);
        }
        self.clone()
    }

    /// Spawns a new thread that invokes the JS `task` callback.
    ///
    /// The callback runs on the Node event loop (via a `ThreadSafeFunction`); the
    /// spawned OS thread blocks until the callback completes. Returns a
    /// [`JoinHandle`] that can be joined.
    #[napi]
    pub fn spawn(&self, task: Function<'_, (), ()>) -> Result<JoinHandle> {
        // The JS `task` function is not `Send`; `build_threadsafe_function` wraps it
        // in a `ThreadSafeFunction` (which *is* `Send + Sync`). The OS thread only
        // ever triggers the call through that TSFN — the JS value itself never
        // crosses the thread boundary.
        let tsfn = task.build_threadsafe_function::<()>().build()?;

        let mut builder = std_thread::Builder::new();
        if let Some(name) = &self.name {
            builder = builder.name(name.clone());
        }
        if let Some(size) = self.stack_size {
            builder = builder.stack_size(size);
        }

        let handle = builder
            .spawn(move || {
                // Blocking: the OS thread waits for the JS callback to finish.
                let _ = tsfn.call((), ThreadsafeFunctionCallMode::Blocking);
            })
            .map_err(|e| {
                Error::new(
                    Status::GenericFailure,
                    format!("failed to spawn thread: {e}"),
                )
            })?;

        let thread = handle.thread().clone();
        Ok(JoinHandle {
            inner: Some(handle),
            thread,
        })
    }
}

/// An owned permission to join a spawned thread.
///
/// Exposed to JS as the `JoinHandle` class.
#[napi]
pub struct JoinHandle {
    inner: Option<std_thread::JoinHandle<()>>,
    thread: std_thread::Thread,
}

#[napi]
impl JoinHandle {
    /// Returns a handle to the underlying thread.
    #[napi]
    pub fn thread(&self) -> Thread {
        Thread {
            inner: self.thread.clone(),
        }
    }

    /// Blocks the calling thread until the spawned thread exits.
    ///
    /// # Deadlock warning
    ///
    /// If called on the Node event-loop thread, this will deadlock whenever the
    /// spawned thread is waiting for its JS callback to run (the event loop is
    /// what runs it). Prefer [`JoinHandle::join_async`].
    #[napi]
    pub fn join(&mut self) -> Result<()> {
        let handle = self.inner.take().ok_or_else(|| {
            Error::new(Status::GenericFailure, "thread already joined".to_string())
        })?;
        handle.join().map(|_| ()).map_err(|_| {
            Error::new(
                Status::GenericFailure,
                "spawned thread panicked".to_string(),
            )
        })
    }

    /// Asynchronously waits for the spawned thread to exit, then invokes `done`.
    ///
    /// Deadlock-free: the blocking `join` runs on a helper OS thread (not the
    /// event loop), so the JS `task` callback is free to execute. `done` runs on
    /// the event loop after `task` has completed.
    #[napi]
    pub fn join_async(&mut self, done: Function<'_, (), ()>) -> Result<()> {
        let handle = self.inner.take().ok_or_else(|| {
            Error::new(Status::GenericFailure, "thread already joined".to_string())
        })?;
        let done_tsfn = done.build_threadsafe_function::<()>().build()?;

        std_thread::spawn(move || {
            let _ = handle.join();
            let _ = done_tsfn.call((), ThreadsafeFunctionCallMode::NonBlocking);
        });

        Ok(())
    }
}

/// Convert a `std::thread::ThreadId` into its raw `u64`.
///
/// `ThreadId` is a `#[repr(?)]` newtype around `NonZero<u64>` with a single
/// 8-byte field, so this transmute is sound.
fn thread_id_to_u64(id: StdThreadId) -> u64 {
    const _: () = assert!(std::mem::size_of::<StdThreadId>() == 8);
    // SAFETY: `ThreadId` is a transparent single-field newtype over `u64`.
    unsafe { std::mem::transmute::<StdThreadId, u64>(id) }
}

/// Maximum number of threads allowed in the libuv thread pool.
///
/// Mirrors libuv's internal `MAX_THREADPOOL_SIZE` (the `UV_THREADPOOL_SIZE`
/// ceiling). libuv does not expose this constant through its Rust bindings, so
/// it is re-stated here.
const MAX_THREADPOOL_SIZE: u32 = 1024;

/// A handle to libuv's global thread pool.
///
/// libuv maintains a single, process-wide thread pool (sized by the
/// `UV_THREADPOOL_SIZE` environment variable, default 4, maximum
/// [`MAX_THREADPOOL_SIZE`]) that is shared across all event loops. It backs
/// `uv_queue_work`, filesystem operations, and `getaddrinfo`/`getnameinfo`
/// requests.
///
/// This class exposes the pool's configuration and is used internally by other
/// bindings (e.g. [`crate::Scheduler`]) to schedule work onto the pool via
/// the default loop.
#[napi]
pub struct ThreadPool {
    loop_: Loop,
}

#[napi]
impl ThreadPool {
    /// Create a handle bound to the default (Node) libuv loop. The underlying
    /// thread pool is global and shared; this only wraps the loop that reports
    /// completion back to JavaScript.
    #[napi(constructor)]
    pub fn new() -> Result<Self> {
        let loop_ = Loop::default().map_err(|e| {
            Error::new(
                Status::GenericFailure,
                format!("failed to get default loop: {e:?}"),
            )
        })?;
        Ok(ThreadPool { loop_ })
    }

    /// The maximum number of threads allowed in the libuv thread pool.
    #[napi]
    pub fn max_size() -> u32 {
        MAX_THREADPOOL_SIZE
    }

    /// The default size of the libuv thread pool when `UV_THREADPOOL_SIZE` is
    /// not set.
    #[napi]
    pub fn default_size() -> u32 {
        4
    }

    /// The effective size of the libuv thread pool, read from the
    /// `UV_THREADPOOL_SIZE` environment variable (capped at [`ThreadPool::max_size`],
    /// minimum 1). Falls back to [`ThreadPool::default_size`] when unset or invalid.
    #[napi]
    pub fn size() -> u32 {
        std::env::var("UV_THREADPOOL_SIZE")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .map(|n| n.clamp(1, MAX_THREADPOOL_SIZE))
            .unwrap_or(ThreadPool::default_size())
    }

    /// The loop whose thread pool this handle drives. Exposed for use by other
    /// bindings (e.g. `Scheduler`) that schedule work onto the pool.
    pub(crate) fn r#loop(&self) -> &Loop {
        &self.loop_
    }

    /// Schedule a Task (`work`/`after`) onto the libuv thread pool, returning the
    /// `WorkReq` request. Internal counterpart to `Loop::queue_work` that keeps
    /// all work on this handle's loop.
    pub(crate) fn run<F, G>(&self, work: F, after: G) -> Result<WorkReq>
    where
        F: Into<WorkCB<'static>>,
        G: Into<AfterWorkCB<'static>>,
    {
        self.loop_
            .queue_work(work, after)
            .map_err(|e| Error::new(Status::GenericFailure, format!("queue_work failed: {e:?}")))
    }

    /// Alias for [`ThreadPool::exec`] — reads naturally at call sites
    /// (`pool.run(work, after)`).
    pub(crate) fn run<F, G>(&self, work: F, after: G) -> Result<WorkReq>
    where
        F: Into<WorkCB<'static>>,
        G: Into<AfterWorkCB<'static>>,
    {
        self.run(work, after)
    }
    /// Run `work` on a libuv worker thread (`uv_queue_work`), then wake the
    /// loop via a `uv_async_t` and invoke `on_loop(result, cancelled)` on the
    /// loop thread. `cancelled` is derived from the `after_work_cb` status:
    /// libuv reports `UV_ECANCELED` there when the request was cancelled with
    /// `uv_cancel` before the work started. `on_loop` receives no `Env` —
    /// settle a `JsDeferred` there and napi supplies the `Env` when the
    /// resolver runs. Returns the `uv_work_t` request so callers can issue
    /// `uv_cancel`. This is the shared worker→loop delivery primitive used by
    /// `task.rs` and `promise.rs`.
    pub(crate) fn exec_then<F, G, T>(&self, work: F, on_loop: G) -> Result<WorkReq>
    where
        F: FnOnce() -> T + Send + 'static,
        G: FnOnce(T, bool) + 'static,
        T: Send + 'static,
    {
        let slot: Arc<Mutex<Option<T>>> = Arc::new(Mutex::new(None));
        let cancelled_slot: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
        let work_slot = Arc::clone(&slot);
        let work_cancelled = Arc::clone(&cancelled_slot);
        let deliver_slot = Arc::clone(&slot);
        let deliver_cancelled = Arc::clone(&cancelled_slot);
        let mut work_opt: Option<F> = Some(work);
        let mut on_loop_opt: Option<G> = Some(on_loop);
        let wake = crate::task::TaskSignal::wake_on(self.r#loop(), move || {
            if let Some(r) = deliver_slot.lock().unwrap().take() {
                let cancelled = *deliver_cancelled.lock().unwrap();
                if let Some(deliver) = on_loop_opt.take() {
                    deliver(r, cancelled);
                }
            }
        })?;
        let work_cb = move |_req: WorkReq| {
            if let Some(w) = work_opt.take() {
                *work_slot.lock().unwrap() = Some(w());
            }
        };
        let after_cb = move |_req: WorkReq, status: std::result::Result<u32, libuv::Error>| {
            // An error status (UV_ECANCELED) means `uv_cancel` took effect.
            *work_cancelled.lock().unwrap() = status.is_err();
            let _ = wake.send();
            let _ = wake.close();
        };
        self.run(work_cb, after_cb)
    }
}
