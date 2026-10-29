use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering as AtomicOrdering};

use napi::{Env, JsFunction, JsValue, Unknown};
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};
use crate::repository::Repository;
use crate::worktree::{StreamEntry, WorktreeHeadKind};

/// Options for constructing a threaded [`Task`].
#[napi(object)]
pub struct TaskOptions {
    /// Optional name for the worker thread (forwarded to `Worker`'s `name`
    /// option). Useful for debugging / profiling under `node --trace-warnings`.
    pub name: Option<String>,
}

/// A threaded task: a wrapper around a Node.js worker thread
/// (`node:worker_threads`) that runs a named script in the background, separate
/// from the main (UI) thread.
///
/// This mirrors the Web Worker / `Worker` model: construct it with a script
/// path, send data with [`Task::post_message`], receive results via the
/// `onmessage` handler, and tear it down with [`Task::terminate`]. Messages are
/// structured-cloned (copied, not shared) between the main thread and the task
/// thread — see `postMessage` / `onmessage` below.
///
/// ```ts
/// const myTask = new Task("task.js");
/// myTask.onmessage = (e) => console.log(e.data);
/// myTask.postMessage([1, 2, 3]);
/// ```
#[napi]
pub struct Task {
    /// Process-unique id used to key the worker and its handlers on the JS side.
    id: u32,
    /// The script path the worker runs.
    script: String,
    /// The `Worker.threadId` assigned by Node when the worker starts.
    thread_id: u32,
    /// Whether [`Task::terminate`] has been called.
    terminated: Cell<bool>,
}

/// Monotonic, process-unique id for each [`Task`] instance.
static NEXT_TASK_ID: AtomicU32 = AtomicU32::new(1);

/// Bootstrap JavaScript installed once per process. It defines global helpers
/// that create/own the `Worker` instances and forward their `message` / `error`
/// / `messageerror` events to per-task handlers. The Rust side only ever talks
/// to these helpers (so arbitrary JS message values never have to cross the
/// napi boundary as `Send` data — they stay on the JS side, structured-cloned
/// by `node:worker_threads` itself).
///
/// It relies on `globalThis.__require` (a `require` made available by the JS
/// wrapper, e.g. `globalThis.__require = require` in `index.ts`, since `require`
/// is not a global inside `run_script`).
const TASK_BOOTSTRAP: &str = r#"
(function () {
  if (typeof globalThis.__taskInit === 'function') return;
  const wt = globalThis.__require('node:worker_threads');
  const workers = (globalThis.__taskWorkers = {});
  const handlers = (globalThis.__taskHandlers = {});
  globalThis.__taskInit = function (id, path, name) {
    const w = new wt.Worker(path, name ? { name: name } : undefined);
    workers[id] = w;
    const fire = (type, makeEvent) => {
      const h = handlers[id] || {};
      const evt = makeEvent();
      if (typeof h[type] === 'function') { try { h[type](evt); } catch (e) {} }
      const list = (h.listeners && h.listeners[type]) || [];
      for (const l of list) { try { l(evt); } catch (e) {} }
    };
    w.on('message', (d) => fire('message', () => ({ data: d })));
    w.on('error', (e) => fire('error', () => ({ error: e, message: e && e.message })));
    w.on('messageerror', (e) => fire('messageerror', () => ({ error: e })));
    return w.threadId;
  };
  globalThis.__taskPost = function (id, msg, transfer) {
    const w = workers[id]; if (!w) return;
    w.postMessage(msg, transfer === undefined ? undefined : (Array.isArray(transfer) ? transfer : [transfer]));
  };
  globalThis.__taskTerminate = function (id) {
    const w = workers[id]; if (!w) return Promise.resolve();
    return w.terminate();
  };
  globalThis.__taskRef = function (id) { const w = workers[id]; if (w && w.ref) w.ref(); };
  globalThis.__taskUnref = function (id) { const w = workers[id]; if (w && w.unref) w.unref(); };
  globalThis.__taskSetHandler = function (id, type, fn) {
    const h = handlers[id] || (handlers[id] = { listeners: {} });
    h[type] = fn;
  };
  globalThis.__taskGetHandler = function (id, type) {
    const h = handlers[id]; return h ? h[type] : undefined;
  };
  globalThis.__taskAddListener = function (id, type, fn) {
    const h = handlers[id] || (handlers[id] = { listeners: {} });
    (h.listeners[type] || (h.listeners[type] = [])).push(fn);
  };
  globalThis.__taskRemoveListener = function (id, type, fn) {
    const h = handlers[id]; if (!h || !h.listeners || !h.listeners[type]) return;
    h.listeners[type] = h.listeners[type].filter((l) => l !== fn);
  };
  globalThis.__taskDispatch = function (id, evt) {
    const h = handlers[id] || {};
    const t = evt && evt.type; if (!t) return;
    if (typeof h[t] === 'function') { try { h[t](evt); } catch (e) {} }
    const list = (h.listeners && h.listeners[t]) || [];
    for (const l of list) { try { l(evt); } catch (e) {} }
  };
})();
"#;

fn task_ensure_bootstrap(env: &Env) -> napi::Result<()> {
    let _: Unknown = env.run_script(TASK_BOOTSTRAP)?;
    Ok(())
}

fn task_global_fn(env: &Env, name: &str) -> napi::Result<JsFunction> {
    env.run_script::<_, JsFunction>(format!("globalThis.{name}"))
}

fn task_undefined(env: &Env) -> napi::Result<Unknown> {
    env.run_script::<_, Unknown>("undefined")
}

#[napi]
impl Task {
    /// Create a task from either:
    /// - a JavaScript `Promise` (mandatory work), or
    /// - a script path `string` (e.g. `"build.ts"`) that is executed via the
    ///   appropriate runtime (`tsx` for TypeScript, `node` for JavaScript)
    ///   when the task runs.
    ///
    /// `options` is optional and currently carries the desired thread count /
    /// name.
    #[napi(constructor)]
    pub fn new(work: Either<String, Promise<()>>, options: Option<TaskOptions>) -> Self {
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        let (work_promise, script) = match work {
            Either::A(script) => (None, Some(script)),
            Either::B(promise) => (Some(promise), None),
        };
        Task {
            work: work_promise,
            script,
            options: options.unwrap_or(TaskOptions {
                threads: None,
                name: None,
            }),
            on_finally: None,
            state: TaskState::Pending,
            cancel_tx: Some(cancel_tx),
            cancel_rx: Some(cancel_rx),
            native_work: None,
            cancel_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Execute the task: drive the wrapped promise to completion and resolve
    /// once it settles. Returns a `Promise<void>` mirroring the work's outcome
    /// (rejecting if the underlying promise rejected).
    ///
    /// If [`cancel`](Self::cancel) is requested while running, `start` detaches
    /// from the in-flight work and resolves with the task marked `Cancelled`.
    #[napi(ts_return_type = "Promise<void>")]
    pub async fn start(&mut self) -> napi::Result<()> {
        if self.state == TaskState::Cancelled {
            return Ok(());
        }
        // Materialize the work future: a wrapped JS promise, or a script path to
        // execute (`node <script>`) on the async runtime's blocking pool.
        let work: std::pin::Pin<Box<dyn std::future::Future<Output = napi::Result<()>> + Send>> =
            if let Some(work) = self.work.take() {
                Box::pin(work)
            } else if let Some(script) = self.script.take() {
                Box::pin(async move {
                    let inner = tokio::task::spawn_blocking(move || run_script_blocking(&script))
                        .await
                        .map_err(|e| {
                            napi::Error::new(
                                napi::Status::GenericFailure,
                                format!("script task join error: {e}"),
                            )
                        })?;
                    inner
                })
            } else {
                return Ok(());
            };
        self.state = TaskState::Running;
        let cancel_rx = self.cancel_rx.take();
        let cancel_fut = async {
            match cancel_rx {
                Some(rx) => {
                    let _ = rx.await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(cancel_fut);
        tokio::select! {
            res = work => {
                self.state = match res {
                    Ok(()) => TaskState::Resolved,
                    Err(_) => TaskState::Rejected,
                };
            }
            _ = &mut cancel_fut => {
                self.state = TaskState::Cancelled;
            }
        }
        if let Some(cb) = &self.on_finally {
            let _ = cb.call(
                (),
                napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
            );
        }
        Ok(())
    }

    /// Request cancellation of a `Pending` or `Running` task.
    ///
    /// Returns `true` if the task transitioned to `Cancelled`. Note that an
    /// arbitrary JavaScript `Promise` (e.g. `cli.build()`) cannot be force-
    /// aborted from Rust: this detaches `start` from awaiting it and marks the
    /// task `Cancelled`, but the underlying work (a child process, etc.) may
    /// keep running until it finishes on its own.
    #[napi]
    pub fn cancel(&mut self) -> napi::Result<bool> {
        if self.state == TaskState::Pending || self.state == TaskState::Running {
            self.state = TaskState::Cancelled;
            self.cancel_flag.store(true, Ordering::SeqCst);
            if let Some(tx) = self.cancel_tx.take() {
                let _ = tx.send(());
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Current lifecycle [`TaskState`] of the task.
    #[napi]
    pub fn status(&self) -> TaskState {
        self.state
    }

    /// The blocking/CPU entry point, mirroring `napi::Task::compute`. For a
    /// promise-based task this simply drives the work to completion; heavy
    /// synchronous work should instead be offloaded to the libuv thread pool.
    #[napi(ts_return_type = "Promise<void>")]
    pub async fn compute(&mut self) -> napi::Result<()> {
        self.start().await
    }

    /// Resolve with `value` — the `Promise.resolve` equivalent.
    #[napi(ts_return_type = "Promise<unknown>")]
    pub async fn resolve(_env: Env, value: serde_json::Value) -> napi::Result<serde_json::Value> {
        Ok(value)
    }

    /// Reject with `reason` — the `Promise.reject` equivalent.
    #[napi(ts_return_type = "Promise<unknown>")]
    pub async fn reject(reason: serde_json::Value) -> napi::Result<serde_json::Value> {
        Err(napi::Error::new(
            napi::Status::GenericFailure,
            serde_json::to_string(&reason).unwrap_or_else(|_| "task rejected".to_string()),
        ))
    }

    /// Register `callback` to run once the task settles, like `Promise.finally`.
    ///
    /// The callback is invoked through a threadsafe function so it is safe to
    /// fire from the task's completion context. Returns the task for chaining.
    pub fn finally(
        &mut self,
        env: Env,
        callback: napi::bindgen_prelude::FunctionRef<(), ()>,
    ) -> napi::Result<()> {
        let f = callback.borrow_back(&env)?;
        let tsfn = f
            .build_threadsafe_function::<()>()?
            .build()?;
        self.on_finally = Some(tsfn);
        Ok(())
    }
}

impl Task {
    /// Build a task whose work runs on the libuv thread pool (not the JS event
    /// loop). `work` is synchronous Rust code executed in `compute()` on a
    /// worker thread — this is the "threads instead of processes" path. JS
    /// cannot supply Rust compute, so threaded tasks are constructed in Rust.
    pub fn with_compute<F>(work: F) -> Self
    where
        F: FnOnce() -> napi::Result<()> + Send + 'static,
    {
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        Task {
            work: None,
            script: None,
            options: TaskOptions {
                threads: None,
                name: None,
            },
            on_finally: None,
            state: TaskState::Pending,
            cancel_tx: Some(cancel_tx),
            cancel_rx: Some(cancel_rx),
            native_work: Some(Box::new(work)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl napi::Task for Task {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> napi::Result<()> {
        self.state = TaskState::Running;
        if self.cancel_flag.load(Ordering::SeqCst) {
            self.state = TaskState::Cancelled;
            return Ok(());
        }
        if let Some(work) = self.native_work.take() {
            work()?;
        } else if let Some(script) = self.script.take() {
            run_script_blocking(&script)?;
        }
        Ok(())
    }

    fn resolve(&mut self, _env: Env, _output: ()) -> napi::Result<()> {
        self.state = TaskState::Resolved;
        Ok(())
    }

    fn reject(&mut self, _env: Env, err: napi::Error) -> napi::Result<()> {
        self.state = TaskState::Rejected;
        Err(err)
    }

    fn finally(&mut self, _env: Env) -> napi::Result<()> {
        if let Some(cb) = &self.on_finally {
            let _ = cb.call(
                (),
                napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
            );
        }
        Ok(())
    }
}

/// Dispatch a [`Task`] (built with native compute via [`Task::with_compute`]) to
/// the libuv thread pool. Returns a promise that resolves once `compute()`
/// finishes on a worker thread. Use this instead of [`Task::start`] when the
/// work is Rust-native and should run on threads rather than as a subprocess.
#[napi]
pub fn run_task_on_pool(task: Task) -> napi::Result<AsyncTask<Task>> {
    Ok(AsyncTask::new(task))
}

/// Returns `true` for TypeScript-style script extensions.
fn is_typescript(script: &str) -> bool {
    let lower = script.to_ascii_lowercase();
    lower.ends_with(".ts")
        || lower.ends_with(".mts")
        || lower.ends_with(".cts")
        || lower.ends_with(".tsx")
}

/// Run a script file to completion, waiting for it to exit. This blocks the
/// calling thread, so it is safe to call from a libuv worker thread (`compute`)
/// or wrapped in `tokio::task::spawn_blocking`.
///
/// The runtime is chosen by extension: `.js`/`.mjs`/`.cjs` run via `node`;
/// `.ts`/`.mts`/`.cts`/`.tsx` run via `tsx`, with a fallback to `npx tsx` if
/// `tsx` is not on `PATH`.
fn run_script_blocking(script: &str) -> napi::Result<()> {
    let runners: &[&[&str]] = if is_typescript(script) {
        &[&["tsx"], &["npx", "tsx"]]
    } else {
        &[&["node"]]
    };

    for runner in runners {
        let (program, rest) = runner.split_first().unwrap();
        let mut cmd = std::process::Command::new(program);
        cmd.args(rest).arg(script);
        match cmd.status() {
            Ok(status) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(napi::Error::new(
                        napi::Status::GenericFailure,
                        format!("script `{script}` exited with status {status}"),
                    ))
                };
            }
            // `tsx` not on PATH → try the next candidate (`npx tsx`).
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(napi::Error::new(
                    napi::Status::GenericFailure,
                    format!("failed to run `{} {script}`: {e}", runner.join(" ")),
                ));
            }
        }
    }

    Err(napi::Error::new(
        napi::Status::GenericFailure,
        format!(
            "could not find a runner for `{script}` (tried: {})",
            runners
                .iter()
                .map(|r| r.join(" "))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    ))
}

/// Worktree materialisation task running on the libuv thread pool.
pub struct StreamTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) tree_id: String,
}

impl StreamTask {
    pub fn new(repo: Arc<gix::ThreadSafeRepository>, tree_id: String) -> Self {
        Self { repo, tree_id }
    }
}

impl napi::Task for StreamTask {
    type Output = Vec<StreamEntry>;
    type JsValue = Vec<StreamEntry>;

    fn compute(&mut self) -> napi::Result<Vec<StreamEntry>> {
        let r = thread_local(&self.repo);
        let id = parse_oid(&self.tree_id)?;
        let (mut stream, _index) = r
            .worktree_stream(id)
            .map_err(|e| err("worktree_stream", e))?;
        let mut out = Vec::new();
        while let Some(mut entry) = stream.next_entry().map_err(|e| err("stream_entry", e))? {
            let path = String::from_utf8_lossy(entry.relative_path().as_ref()).into_owned();
            let mode = format!("{:?}", entry.mode);
            let id = entry.id.to_string();
            let size = entry.bytes_remaining().map(|n| n as f64);
            std::io::copy(&mut entry, &mut std::io::sink())
                .map_err(|e| err("stream_entry_read", e))?;
            out.push(StreamEntry {
                path,
                mode,
                id,
                size,
            });
        }
        Ok(out)
    }

    fn resolve(&mut self, _env: Env, output: Vec<StreamEntry>) -> napi::Result<Vec<StreamEntry>> {
        Ok(output)
    }
}

/// Worktree-stream raw-bytes task running on the libuv thread pool.
pub struct StreamRawTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) tree_id: String,
}

impl StreamRawTask {
    pub fn new(repo: Arc<gix::ThreadSafeRepository>, tree_id: String) -> Self {
        Self { repo, tree_id }
    }
}

impl napi::Task for StreamRawTask {
    type Output = Vec<u8>;
    type JsValue = Buffer;

    fn compute(&mut self) -> napi::Result<Vec<u8>> {
        let r = thread_local(&self.repo);
        let id = parse_oid(&self.tree_id)?;
        let (stream, _index) = r
            .worktree_stream(id)
            .map_err(|e| err("worktree_stream", e))?;
        let mut read = stream.into_read();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut read, &mut bytes).map_err(|e| err("stream_encode", e))?;
        Ok(bytes)
    }

    fn resolve(&mut self, _env: Env, output: Vec<u8>) -> napi::Result<Buffer> {
        Ok(Buffer::from(output))
    }
}

/// Worktree addition task running on the libuv thread pool.
pub struct AddWorktreeTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) destination: String,
    pub(crate) head_kind: WorktreeHeadKind,
    pub(crate) head_name: String,
}

impl AddWorktreeTask {
    pub fn new(
        repo: Arc<gix::ThreadSafeRepository>,
        destination: String,
        head_kind: WorktreeHeadKind,
        head_name: String,
    ) -> Self {
        Self {
            repo,
            destination,
            head_kind,
            head_name,
        }
    }
}

impl napi::Task for AddWorktreeTask {
    type Output = Repository;
    type JsValue = Repository;

    fn compute(&mut self) -> napi::Result<Repository> {
        let r = thread_local(&self.repo);
        let head = match self.head_kind {
            WorktreeHeadKind::Detached => {
                gix::worktree::add::Head::Detached(parse_oid(&self.head_name)?)
            }
            WorktreeHeadKind::Attached => {
                let full = if self.head_name.starts_with("refs/") {
                    self.head_name.clone()
                } else {
                    format!("refs/heads/{}", self.head_name)
                };
                let name =
                    gix::refs::FullName::try_from(full.as_str()).map_err(|e| err("ref_name", e))?;
                gix::worktree::add::Head::Attached(name)
            }
        };
        let (repo, _outcome) = r
            .add_worktree(
                self.destination.as_str(),
                head,
                gix::progress::Discard,
                &AtomicBool::new(false),
            )
            .map_err(|e| err("add_worktree", e))?;
        Ok(Repository {
            inner: Arc::new(repo.into()),
        })
    }

    fn resolve(&mut self, _env: Env, output: Repository) -> napi::Result<Repository> {
        Ok(output)
    }
}

/// Worktree removal task running on the libuv thread pool.
pub struct RemoveWorktreeTask {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
    pub(crate) target: String,
    pub(crate) force: gix::worktree::remove::Force,
}

impl RemoveWorktreeTask {
    pub fn new(
        repo: Arc<gix::ThreadSafeRepository>,
        target: String,
        force: gix::worktree::remove::Force,
    ) -> Self {
        Self { repo, target, force }
    }
}

impl napi::Task for RemoveWorktreeTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> napi::Result<()> {
        let r = thread_local(&self.repo);
        r.remove_worktree(
            std::path::Path::new(&self.target),
            self.force,
            gix::progress::Discard,
        )
        .map_err(|e| err("remove_worktree", e))
    }

    fn resolve(&mut self, _env: Env, output: ()) -> napi::Result<()> {
        Ok(output)
    }
}
