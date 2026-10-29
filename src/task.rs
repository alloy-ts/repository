use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use napi::{Env, FromNapiValue, NapiRaw, Unknown, ValueType};
use napi::bindgen_prelude::{Buffer, JsFunction, ToNapiValue};
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};
use crate::repository::Repository;
use crate::worktree::{StreamEntry, WorktreeHeadKind};

/// Resource limits forwarded to the underlying `node:worker_threads` `Worker`.
///
/// Mirrors the C++ `Task` `resource_limits_` float array (`node_worker.h`):
/// `maxYoungGenerationSizeMb`, `maxOldGenerationSizeMb`, `codeRangeSizeMb`,
/// `stackSizeMb`. `None` means "inherit Node's default".
#[napi(object)]
pub struct TaskResourceLimits {
    #[napi(js_name = "maxYoungGenerationSizeMb")]
    pub max_young_generation_size_mb: Option<u32>,
    #[napi(js_name = "maxOldGenerationSizeMb")]
    pub max_old_generation_size_mb: Option<u32>,
    #[napi(js_name = "codeRangeSizeMb")]
    pub code_range_size_mb: Option<u32>,
    #[napi(js_name = "stackSizeMb")]
    pub stack_size_mb: Option<u32>,
}

/// Options for constructing a threaded [`Task`] (a `node:worker_threads` worker).
#[napi(object)]
pub struct TaskOptions {
    /// Optional name for the worker thread (forwarded to `Worker`'s `name` option).
    pub name: Option<String>,
    /// V8 resource limits (heap / code-range / stack size).
    #[napi(js_name = "resourceLimits")]
    pub resource_limits: Option<TaskResourceLimits>,
    /// Per-worker `process.env`. Accepts `'inherit'`, `'none'`, or a plain object.
    /// (Advanced `SHARE_ENV` symbol support may require passing it via
    /// `worker_data` instead, since it cannot round-trip through JSON.)
    pub env: Option<serde_json::Value>,
    /// Extra CLI args exposed as `process.argv` inside the worker.
    pub argv: Option<Vec<String>>,
    /// Extra Node CLI args (e.g. `--max-old-space-size`) for the worker.
    #[napi(js_name = "execArgv")]
    pub exec_argv: Option<Vec<String>>,
    /// Arbitrary data made available to the worker as `workerData`.
    #[napi(js_name = "workerData")]
    pub worker_data: Option<serde_json::Value>,
    /// Forward unmanaged file descriptors into the worker.
    #[napi(js_name = "trackUnmanagedFds")]
    pub track_unmanaged_fds: Option<bool>,
}

/// A threaded task: a thin Rust wrapper around a Node.js worker thread
/// (`node:worker_threads`) running a named script off the main (UI) thread.
///
/// This mirrors the Web Worker / `Worker` model: construct it with a script
/// path, send data with [`Task::post_message`], receive results via the
/// `onmessage` handler, and tear it down with [`Task::terminate`]. Messages are
/// structured-cloned between the main thread and the task thread.
///
/// ```ts
/// const myTask = new Task("task.js", { name: "build" });
/// myTask.onmessage = (e) => console.log(e.data);
/// myTask.postMessage([1, 2, 3]);
/// ```
///
/// The binding does **not** reimplement Node's internal `Task` C++ class; it
/// delegates to the public `Worker`, which already runs that machinery (a fresh
/// V8 isolate + libuv loop per worker). The JS-side glue lives in
/// [`TASK_BOOTSTRAP`] below.
#[napi]
pub struct Task {
    /// Process-unique id used to key the worker and its handlers on the JS side.
    id: u32,
    /// The `Worker.threadId` assigned by Node when the worker starts.
    thread_id: u32,
    /// Whether [`Task::terminate`] has been called.
    terminated: Cell<bool>,
}

/// Monotonic, process-unique id for each [`Task`] instance.
static NEXT_TASK_ID: AtomicU32 = AtomicU32::new(1);

/// Bootstrap JavaScript installed once per process. It defines global helpers
/// that create/own the `Worker` instances and forward their `message` / `error`
/// / `messageerror` / `exit` events to per-task handlers. The Rust side only
/// ever talks to these helpers — arbitrary JS message values never cross the
/// napi boundary as `Send` data; they stay on the JS side, structured-cloned by
/// `node:worker_threads` itself.
///
/// Relies on `globalThis.__require` (a `require` exposed by the JS host, e.g.
/// `globalThis.__require = require` in `index.ts`, since `require` is not a
/// global inside `run_script`).
const TASK_BOOTSTRAP: &str = r#"
(function () {
  if (typeof globalThis.__taskInit === 'function') return;
  const wt = globalThis.__require('node:worker_threads');
  const workers = (globalThis.__taskWorkers = {});
  const handlers = (globalThis.__taskHandlers = {});
  function fire(id, type, makeEvent) {
    const h = handlers[id] || {};
    const evt = makeEvent();
    if (typeof h[type] === 'function') { try { h[type](evt); } catch (e) {} }
    const list = (h.listeners && h.listeners[type]) || [];
    for (const l of list) { try { l(evt); } catch (e) {} }
  }
  globalThis.__taskInit = function (id, path, options) {
    const o = options || {};
    const w = new wt.Worker(path, {
      name: o.name ?? undefined,
      resourceLimits: o.resourceLimits ? {
        maxYoungGenerationSizeMb: o.resourceLimits.maxYoungGenerationSizeMb ?? undefined,
        maxOldGenerationSizeMb: o.resourceLimits.maxOldGenerationSizeMb ?? undefined,
        codeRangeSizeMb: o.resourceLimits.codeRangeSizeMb ?? undefined,
        stackSizeMb: o.resourceLimits.stackSizeMb ?? undefined,
      } : undefined,
      argv: o.argv ?? undefined,
      execArgv: o.execArgv ?? undefined,
      workerData: o.workerData ?? undefined,
      env: o.env ?? undefined,
      trackUnmanagedFds: o.trackUnmanagedFds ?? undefined,
    });
    workers[id] = w;
    w.on('message', (d) => fire(id, 'message', () => ({ data: d })));
    w.on('error', (e) => fire(id, 'error', () => ({ error: e, message: e && e.message })));
    w.on('messageerror', (e) => fire(id, 'messageerror', () => ({ error: e })));
    w.on('exit', (code, sig) => fire(id, 'exit', () => ({ code, signal: sig })));
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
  globalThis.__taskGetHeapSnapshot = function (id, opts) {
    const w = workers[id]; if (!w) return undefined;
    return w.getHeapSnapshot(opts);
  };
  globalThis.__taskGetHeapStatistics = function (id) {
    const w = workers[id]; if (!w) return undefined;
    return w.getHeapStatistics();
  };
  globalThis.__taskCpuUsage = function (id, prev) {
    const w = workers[id]; if (!w) return undefined;
    return w.cpuUsage(prev);
  };
  globalThis.__taskEventLoopUtilization = function (id, prev) {
    const w = workers[id]; if (!w) return undefined;
    return w.performance.eventLoopUtilization(prev);
  };
  globalThis.__taskStartCpuProfile = function (id, opts) {
    const w = workers[id]; if (!w) return undefined;
    return w.startCpuProfile(opts);
  };
  globalThis.__taskStopCpuProfile = function (id, opts) {
    const w = workers[id]; if (!w) return undefined;
    return w.stopCpuProfile(opts);
  };
  globalThis.__taskStartHeapProfile = function (id, opts) {
    const w = workers[id]; if (!w) return undefined;
    return w.startHeapProfile(opts);
  };
  globalThis.__taskStopHeapProfile = function (id, opts) {
    const w = workers[id]; if (!w) return undefined;
    return w.stopHeapProfile(opts);
  };
})();
"#;

fn task_ensure_bootstrap(env: &Env) -> napi::Result<()> {
    let _: Unknown<'_> = env.run_script(TASK_BOOTSTRAP)?;
    Ok(())
}

/// Convert any `ToNapiValue` (a JS value owned by Rust) into an `Unknown`,
/// keeping the napi value alive on the JS side. Rebinds the lifetime to
/// `'static` so the value can be returned from `#[napi]` methods.
fn to_unknown<T: ToNapiValue>(env: &Env, val: T) -> napi::Result<Unknown<'static>> {
    let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), val)? };
    Ok(Unknown::from_raw(env.raw(), raw)?)
}

/// Produce the JS `undefined` value as an `Unknown<'static>`.
fn undefined_value(env: &Env) -> napi::Result<Unknown<'static>> {
    let u: Unknown<'_> = env.run_script::<_, Unknown<'_>>("undefined")?;
    let raw = u.raw();
    Ok(Unknown::from_raw(env.raw(), raw)?)
}

#[allow(deprecated)]
fn task_global_fn(env: &Env, name: &str) -> napi::Result<JsFunction<'static>> {
    let f: JsFunction<'_> = env.run_script::<_, JsFunction<'_>>(format!("globalThis.{name}"))?;
    let raw = f.raw();
    Ok(JsFunction::from_raw(env.raw(), raw)?)
}

/// Call a global bootstrap helper, returning its (already-JS) result unchanged.
#[allow(deprecated)]
fn task_call(env: &Env, name: &str, args: &[Unknown<'static>]) -> napi::Result<Unknown<'static>> {
    let func = task_global_fn(env, name)?;
    let result = func.call(None, args)?;
    let raw = result.raw();
    Ok(Unknown::from_raw(env.raw(), raw)?)
}

#[allow(deprecated)]
#[napi]
impl Task {
    /// Create a task that runs `script` (a file path understood by
    /// `node:worker_threads`, e.g. `"build.js"`) on a dedicated worker thread.
    #[napi(constructor)]
    pub fn new(env: Env, script: String, options: Option<TaskOptions>) -> napi::Result<Self> {
        task_ensure_bootstrap(&env)?;
        let id = NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed);

        let opts_unknown = match options {
            Some(o) => to_unknown(&env, o)?,
            None => to_unknown(&env, env.create_object()?)?,
        };
        let script_unknown = to_unknown(&env, env.create_string(&script)?)?;
        let id_unknown = to_unknown(&env, env.create_uint32(id)?)?;

        let func = task_global_fn(&env, "__taskInit")?;
        let args = [id_unknown, script_unknown, opts_unknown];
        let result = func.call(None, &args)?;
        let thread_id = u32::from_napi_value(env.raw(), result.raw())?;

        Ok(Task {
            id,
            thread_id,
            terminated: Cell::new(false),
        })
    }

    /// The `Worker.threadId` assigned to this task (available immediately after
    /// construction). Mirrors the C++ `Task::thread_id_`.
    #[napi(getter)]
    pub fn get_thread_id(&self) -> u32 {
        self.thread_id
    }

    /// Whether [`Task::terminate`] has been called.
    #[napi(getter)]
    pub fn get_terminated(&self) -> bool {
        self.terminated.get()
    }

    /// Send `message` to the worker thread. `transfer` is an optional list of
    /// `ArrayBuffer` / `MessagePort` objects whose ownership is transferred
    /// (zero-copy) rather than cloned.
    #[napi]
    pub fn post_message(
        &self,
        env: Env,
        message: Unknown<'static>,
        transfer: Option<Vec<Unknown<'static>>>,
    ) -> napi::Result<()> {
        let id_unknown = to_unknown(&env, env.create_uint32(self.id)?)?;
        let transfer_unknown = match transfer {
            Some(t) => {
                let arr = env.create_array(t.len() as u32)?;
                for (i, item) in t.into_iter().enumerate() {
                    arr.set_element(i as u32, item)?;
                }
                to_unknown(&env, arr)?
            }
            None => undefined_value(&env)?,
        };
        let args = [id_unknown, message, transfer_unknown];
        task_call(&env, "__taskPost", &args)?;
        Ok(())
    }

    /// Terminate the worker thread. Resolves to the worker's exit code once it
    /// has exited — mirrors C++ `Task::StopThread` / `JoinThread`.
    #[napi(ts_return_type = "Promise<number>")]
    pub fn terminate(&self, env: Env) -> napi::Result<Unknown<'static>> {
        let id_unknown = to_unknown(&env, env.create_uint32(self.id)?)?;
        self.terminated.set(true);
        task_call(&env, "__taskTerminate", &[id_unknown])
    }

    /// Increment the worker's reference count so it keeps the process alive
    /// (default). Mirrors C++ `Task::Ref`.
    #[napi]
    pub fn reference(&self, env: Env) -> napi::Result<()> {
        let id_unknown = to_unknown(&env, env.create_uint32(self.id)?)?;
        task_call(&env, "__taskRef", &[id_unknown])?;
        Ok(())
    }

    /// Decrement the worker's reference count so the process may exit even while
    /// the worker is running. Mirrors C++ `Task::Unref`.
    #[napi]
    pub fn unreference(&self, env: Env) -> napi::Result<()> {
        let id_unknown = to_unknown(&env, env.create_uint32(self.id)?)?;
        task_call(&env, "__taskUnref", &[id_unknown])?;
        Ok(())
    }

    #[napi(getter)]
    pub fn get_onmessage(&self, env: Env) -> napi::Result<Option<JsFunction<'static>>> {
        task_get_handler(&env, self.id, "message")
    }

    #[napi(setter)]
    pub fn set_onmessage(
        &self,
        env: Env,
        handler: Option<JsFunction<'static>>,
    ) -> napi::Result<()> {
        task_set_handler(&env, self.id, "message", handler)
    }

    #[napi(getter)]
    pub fn get_onerror(&self, env: Env) -> napi::Result<Option<JsFunction<'static>>> {
        task_get_handler(&env, self.id, "error")
    }

    #[napi(setter)]
    pub fn set_onerror(&self, env: Env, handler: Option<JsFunction<'static>>) -> napi::Result<()> {
        task_set_handler(&env, self.id, "error", handler)
    }

    #[napi(getter)]
    pub fn get_onmessageerror(&self, env: Env) -> napi::Result<Option<JsFunction<'static>>> {
        task_get_handler(&env, self.id, "messageerror")
    }

    #[napi(setter)]
    pub fn set_onmessageerror(
        &self,
        env: Env,
        handler: Option<JsFunction<'static>>,
    ) -> napi::Result<()> {
        task_set_handler(&env, self.id, "messageerror", handler)
    }

    /// Register an event listener (mirrors `EventTarget.addEventListener`).
    #[napi]
    pub fn add_event_listener(
        &self,
        env: Env,
        type_: String,
        listener: JsFunction<'static>,
    ) -> napi::Result<()> {
        let id_unknown = to_unknown(&env, env.create_uint32(self.id)?)?;
        let type_unknown = to_unknown(&env, env.create_string(&type_)?)?;
        let listener_unknown = to_unknown(&env, listener)?;
        task_call(
            &env,
            "__taskAddListener",
            &[id_unknown, type_unknown, listener_unknown],
        )?;
        Ok(())
    }

    /// Remove a previously registered event listener.
    #[napi]
    pub fn remove_event_listener(
        &self,
        env: Env,
        type_: String,
        listener: JsFunction<'static>,
    ) -> napi::Result<()> {
        let id_unknown = to_unknown(&env, env.create_uint32(self.id)?)?;
        let type_unknown = to_unknown(&env, env.create_string(&type_)?)?;
        let listener_unknown = to_unknown(&env, listener)?;
        task_call(
            &env,
            "__taskRemoveListener",
            &[id_unknown, type_unknown, listener_unknown],
        )?;
        Ok(())
    }

    /// Dispatch an event to this task's listeners (mirrors `EventTarget.dispatchEvent`).
    #[napi]
    pub fn dispatch_event(&self, env: Env, evt: Unknown<'static>) -> napi::Result<()> {
        let id_unknown = to_unknown(&env, env.create_uint32(self.id)?)?;
        task_call(&env, "__taskDispatch", &[id_unknown, evt])?;
        Ok(())
    }

    /// Capture a heap snapshot of the worker, returned as a Node `Readable`
    /// stream. Mirrors C++ `Task::TakeHeapSnapshot`.
    #[napi(ts_return_type = "Promise<import('stream').Readable>")]
    pub fn get_heap_snapshot(
        &self,
        env: Env,
        options: Option<Unknown<'static>>,
    ) -> napi::Result<Unknown<'static>> {
        task_diag(&env, self.id, "__taskGetHeapSnapshot", options)
    }

    /// Return the worker's V8 heap statistics. Mirrors C++ `Task::GetHeapStatistics`.
    #[napi(ts_return_type = "Promise<object>")]
    pub fn get_heap_statistics(&self, env: Env) -> napi::Result<Unknown<'static>> {
        let id_unknown = to_unknown(&env, env.create_uint32(self.id)?)?;
        task_call(&env, "__taskGetHeapStatistics", &[id_unknown])
    }

    /// CPU time used by the worker so far (`{ user, system }` in microseconds).
    /// Mirrors C++ `Task::CpuUsage`.
    #[napi(ts_return_type = "{ user: number, system: number }")]
    pub fn cpu_usage(
        &self,
        env: Env,
        prev: Option<Unknown<'static>>,
    ) -> napi::Result<Unknown<'static>> {
        task_diag(&env, self.id, "__taskCpuUsage", prev)
    }

    /// Event-loop utilization of the worker. Mirrors C++ `Task::LoopIdleTime`.
    #[napi(ts_return_type = "object")]
    pub fn event_loop_utilization(
        &self,
        env: Env,
        prev: Option<Unknown<'static>>,
    ) -> napi::Result<Unknown<'static>> {
        task_diag(&env, self.id, "__taskEventLoopUtilization", prev)
    }

    /// Start a CPU profile of the worker. Mirrors C++ `Task::StartCpuProfile`.
    #[napi(ts_return_type = "unknown")]
    pub fn start_cpu_profile(
        &self,
        env: Env,
        options: Option<Unknown<'static>>,
    ) -> napi::Result<Unknown<'static>> {
        task_diag(&env, self.id, "__taskStartCpuProfile", options)
    }

    /// Stop a CPU profile; resolves to the profile as a JSON string.
    #[napi(ts_return_type = "Promise<string>")]
    pub fn stop_cpu_profile(
        &self,
        env: Env,
        options: Option<Unknown<'static>>,
    ) -> napi::Result<Unknown<'static>> {
        task_diag(&env, self.id, "__taskStopCpuProfile", options)
    }

    /// Start a heap profile of the worker. Mirrors C++ `Task::StartHeapProfile`.
    #[napi(ts_return_type = "unknown")]
    pub fn start_heap_profile(
        &self,
        env: Env,
        options: Option<Unknown<'static>>,
    ) -> napi::Result<Unknown<'static>> {
        task_diag(&env, self.id, "__taskStartHeapProfile", options)
    }

    /// Stop a heap profile; resolves to the profile as a JSON string.
    #[napi(ts_return_type = "Promise<string>")]
    pub fn stop_heap_profile(
        &self,
        env: Env,
        options: Option<Unknown<'static>>,
    ) -> napi::Result<Unknown<'static>> {
        task_diag(&env, self.id, "__taskStopHeapProfile", options)
    }
}

#[allow(deprecated)]
fn task_set_handler(
    env: &Env,
    id: u32,
    type_: &str,
    handler: Option<JsFunction<'static>>,
) -> napi::Result<()> {
    let id_unknown = to_unknown(&env, env.create_uint32(id)?)?;
    let type_unknown = to_unknown(&env, env.create_string(type_)?)?;
    let handler_unknown = match handler {
        Some(h) => to_unknown(&env, h)?,
        None => undefined_value(&env)?,
    };
    task_call(
        env,
        "__taskSetHandler",
        &[id_unknown, type_unknown, handler_unknown],
    )?;
    Ok(())
}

#[allow(deprecated)]
fn task_get_handler(
    env: &Env,
    id: u32,
    type_: &str,
) -> napi::Result<Option<JsFunction<'static>>> {
    let id_unknown = to_unknown(&env, env.create_uint32(id)?)?;
    let type_unknown = to_unknown(&env, env.create_string(type_)?)?;
    let result = task_call(env, "__taskGetHandler", &[id_unknown, type_unknown])?;
    match result.get_type()? {
        ValueType::Undefined | ValueType::Null => Ok(None),
        _ => {
            let raw = result.raw();
            Ok(Some(JsFunction::from_raw(env.raw(), raw)?))
        }
    }
}

fn task_diag(
    env: &Env,
    id: u32,
    name: &str,
    options: Option<Unknown<'static>>,
) -> napi::Result<Unknown<'static>> {
    let id_unknown = to_unknown(&env, env.create_uint32(id)?)?;
    let opts_unknown = match options {
        Some(o) => o,
        None => undefined_value(&env)?,
    };
    task_call(env, name, &[id_unknown, opts_unknown])
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
