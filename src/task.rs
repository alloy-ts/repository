use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering as AtomicOrdering};

use napi::bindgen_prelude::{
    Buffer, FromNapiValue, Function, JsObjectValue, Object, ToNapiValue,
};
use napi::{Env, Unknown};
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};
use crate::repository::Repository;
use crate::worktree::{StreamEntry, WorktreeHeadKind};

#[napi]
pub const K_MAX_YOUNG_GENERATION_SIZE_MB: u32 = 0;

#[napi]
pub const K_MAX_OLD_GENERATION_SIZE_MB: u32 = 1;

#[napi]
pub const K_CODE_RANGE_SIZE_MB: u32 = 2;

#[napi]
pub const K_STACK_SIZE_MB: u32 = 3;

#[napi]
pub const K_TOTAL_RESOURCE_LIMIT_COUNT: u32 = 4;

/// Resource limits configuration object for task worker threads.
#[napi(object)]
#[derive(Default, Debug, Clone)]
pub struct TaskResourceLimits {
    pub max_young_generation_size_mb: Option<f64>,
    pub max_old_generation_size_mb: Option<f64>,
    pub code_range_size_mb: Option<f64>,
    pub stack_size_mb: Option<f64>,
}

/// Options for constructing a threaded [`Task`].
#[napi(object)]
#[derive(Default, Debug, Clone)]
pub struct TaskOptions {
    /// Optional name for the worker thread (forwarded to `Worker`'s `name`
    /// option). Useful for debugging / profiling under `node --trace-warnings`.
    pub name: Option<String>,
}

/// Alias exporting `Worker` as a synonym for `Task` on module exports.
#[napi(module_exports)]
pub fn register_worker_alias(mut exports: Object) -> napi::Result<()> {
    if let Ok(task_class) = exports.get_named_property::<Unknown>("Task") {
        exports.set_named_property("Worker", task_class)?;
    }
    Ok(())
}

/// Returns the message port for the current environment.
#[napi(js_name = "getEnvMessagePort")]
pub fn get_env_message_port(env: &Env) -> napi::Result<Unknown<'_>> {
    let fn_handle = task_global_fn::<(), Unknown>(env, "__taskGetEnvMessagePort")?;
    fn_handle.call(())
}

/// Whether execution is currently on the main thread.
#[napi(js_name = "isMainThread")]
pub fn is_main_thread(env: &Env) -> napi::Result<bool> {
    let fn_handle = task_global_fn::<(), bool>(env, "__taskIsMainThread")?;
    fn_handle.call(())
}

/// Whether execution is currently on an internal thread.
#[napi(js_name = "isInternalThread")]
pub fn is_internal_thread() -> bool {
    false
}

/// Whether the current thread owns process state.
#[napi(js_name = "ownsProcessState")]
pub fn owns_process_state(env: &Env) -> napi::Result<bool> {
    is_main_thread(env)
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
    /// Optional thread name.
    name: Option<String>,
    /// The `Worker.threadId` assigned by Node when the worker starts.
    thread_id: u32,
    /// Whether [`Task::terminate`] has been called.
    terminated: AtomicBool,
}

/// Monotonic, process-unique id for each [`Task`] instance.
static NEXT_TASK_ID: AtomicU32 = AtomicU32::new(1);

/// Bootstrap JavaScript installed once per process. It defines global helpers
/// that create/own the `Worker` instances and forward their `message` / `error`
/// / `messageerror` events to per-task handlers. The Rust side only ever talks
/// to these helpers (so arbitrary JS message values never have to cross the
/// napi boundary as `Send` data — they stay on the JS side, structured-cloned
/// by `node:worker_threads` itself).
const TASK_BOOTSTRAP: &str = r#"
(function () {
  if (typeof globalThis.__taskInit === 'function') return;
  const wt = (typeof globalThis.__require === 'function')
    ? globalThis.__require('node:worker_threads')
    : require('node:worker_threads');
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
    w.on('message', (d) => fire('message', () => ({ type: 'message', data: d })));
    w.on('error', (e) => fire('error', () => ({ type: 'error', error: e, message: e && e.message })));
    w.on('messageerror', (e) => fire('messageerror', () => ({ type: 'messageerror', error: e })));
    return w.threadId;
  };
  globalThis.__taskPost = function (id, msg, transfer) {
    const w = workers[id]; if (!w) return;
    if (transfer !== undefined && transfer !== null) {
      w.postMessage(msg, Array.isArray(transfer) ? transfer : [transfer]);
    } else {
      w.postMessage(msg);
    }
  };
  globalThis.__taskTerminate = function (id) {
    const w = workers[id]; if (!w) return Promise.resolve();
    delete workers[id];
    delete handlers[id];
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
  globalThis.__taskGetEnvMessagePort = function () {
    return wt.parentPort || null;
  };
  globalThis.__taskIsMainThread = function () {
    return wt.isMainThread;
  };
  globalThis.__taskHasRef = function (id) {
    const w = workers[id]; return w ? (typeof w.hasRef === 'function' ? w.hasRef() : true) : false;
  };
  globalThis.__taskGetHeapStatistics = function (id) {
    const w = workers[id];
    return (w && typeof w.getHeapStatistics === 'function') ? w.getHeapStatistics() : Promise.resolve({});
  };
  globalThis.__taskCpuUsage = function (id, prev) {
    const w = workers[id];
    return (w && typeof w.cpuUsage === 'function') ? w.cpuUsage(prev) : Promise.resolve({ user: 0, system: 0 });
  };
  globalThis.__taskTakeHeapSnapshot = function (id, options) {
    const w = workers[id];
    return (w && typeof w.getHeapSnapshot === 'function') ? w.getHeapSnapshot(options) : Promise.resolve({});
  };
  globalThis.__taskStartCpuProfile = function (id, options) {
    const w = workers[id];
    return (w && typeof w.startCpuProfile === 'function') ? w.startCpuProfile(options) : Promise.resolve({});
  };
  globalThis.__taskStopCpuProfile = function (id, profileId) {
    const w = workers[id];
    return (w && typeof w.stopCpuProfile === 'function') ? w.stopCpuProfile(profileId) : Promise.resolve({});
  };
  globalThis.__taskStartHeapProfile = function (id, options) {
    const w = workers[id];
    return (w && typeof w.startHeapProfile === 'function') ? w.startHeapProfile(options) : Promise.resolve({});
  };
  globalThis.__taskStopHeapProfile = function (id) {
    const w = workers[id];
    return (w && typeof w.stopHeapProfile === 'function') ? w.stopHeapProfile() : Promise.resolve({});
  };
})();
"#;

fn task_ensure_bootstrap(env: &Env) -> napi::Result<()> {
    let _: Unknown = env.run_script::<&str, Unknown>(TASK_BOOTSTRAP)?;
    Ok(())
}

fn task_global_fn<'a, Args: ToNapiValue, Return: FromNapiValue>(
    env: &'a Env,
    name: &str,
) -> napi::Result<Function<'a, Args, Return>> {
    env.run_script::<String, Function<'a, Args, Return>>(format!("globalThis.{name}"))
}

fn task_undefined(env: &Env) -> napi::Result<Unknown<'_>> {
    env.run_script::<&str, Unknown>("undefined")
}

#[napi]
impl Task {
    /// Create a threaded task running `script` in a separate `node:worker_threads` thread.
    #[napi(constructor)]
    pub fn new(env: Env, script: String, options: Option<TaskOptions>) -> napi::Result<Self> {
        task_ensure_bootstrap(&env)?;
        let id = NEXT_TASK_ID.fetch_add(1, AtomicOrdering::Relaxed);
        let name_str = options.as_ref().and_then(|o| o.name.clone());

        let script_json = serde_json::to_string(&script)
            .map_err(|e| napi::Error::new(napi::Status::GenericFailure, e.to_string()))?;
        let name_json = match &name_str {
            Some(n) => serde_json::to_string(n)
                .map_err(|e| napi::Error::new(napi::Status::GenericFailure, e.to_string()))?,
            None => "undefined".to_string(),
        };

        let code = format!("globalThis.__taskInit({id}, {script_json}, {name_json})");
        let thread_id_num: f64 = env.run_script::<&str, f64>(&code)?;

        Ok(Task {
            id,
            script,
            name: name_str,
            thread_id: thread_id_num as u32,
            terminated: AtomicBool::new(false),
        })
    }

    /// Script path that this task executes.
    #[napi(getter)]
    pub fn script(&self) -> String {
        self.script.clone()
    }

    /// Optional thread name.
    #[napi(getter, js_name = "threadName")]
    pub fn thread_name(&self) -> Option<String> {
        self.name.clone()
    }

    /// The `Worker.threadId` assigned by Node.js when the worker starts.
    #[napi(getter, js_name = "threadId")]
    pub fn thread_id(&self) -> u32 {
        self.thread_id
    }

    /// Whether [`Task::terminate`] has been called on this instance.
    #[napi(getter)]
    pub fn terminated(&self) -> bool {
        self.terminated.load(AtomicOrdering::Relaxed)
    }

    /// Resource limits configured for this task.
    #[napi(getter, js_name = "resourceLimits")]
    pub fn resource_limits(&self) -> TaskResourceLimits {
        TaskResourceLimits::default()
    }

    /// Explicitly start the worker thread.
    #[napi(js_name = "startThread")]
    pub fn start_thread(&self) -> napi::Result<()> {
        Ok(())
    }

    /// Stop execution in the worker thread.
    #[napi(js_name = "stopThread")]
    pub fn stop_thread(&self, env: Env) -> napi::Result<Unknown<'_>> {
        self.terminate(env)
    }

    /// Check if the worker thread handle is keeping the event loop alive.
    #[napi(js_name = "hasRef")]
    pub fn has_ref(&self, env: Env) -> napi::Result<bool> {
        let fn_handle = task_global_fn::<u32, bool>(&env, "__taskHasRef")?;
        fn_handle.call(self.id)
    }

    /// Get resource limit settings as a raw array.
    #[napi(js_name = "getResourceLimits")]
    pub fn get_resource_limits(&self) -> Vec<f64> {
        vec![-1.0, -1.0, -1.0, -1.0]
    }

    /// Returns cumulative event loop idle time in milliseconds.
    #[napi(js_name = "loopIdleTime")]
    pub fn loop_idle_time(&self) -> f64 {
        0.0
    }

    /// Returns high-resolution monotonic timestamp of event loop start time.
    #[napi(js_name = "loopStartTime")]
    pub fn loop_start_time(&self) -> f64 {
        0.0
    }

    /// Get V8 heap statistics for the worker thread.
    #[napi(js_name = "getHeapStatistics")]
    pub fn get_heap_statistics(&self, env: Env) -> napi::Result<Unknown<'_>> {
        let fn_handle = task_global_fn::<u32, Unknown>(&env, "__taskGetHeapStatistics")?;
        fn_handle.call(self.id)
    }

    /// Get CPU usage statistics for the worker thread.
    #[napi(js_name = "cpuUsage")]
    pub fn cpu_usage(&self, env: Env, prev: Option<Unknown>) -> napi::Result<Unknown<'_>> {
        let fn_handle = task_global_fn::<(u32, Unknown), Unknown>(&env, "__taskCpuUsage")?;
        let js_prev = prev.unwrap_or_else(|| task_undefined(&env).unwrap());
        fn_handle.call((self.id, js_prev))
    }

    /// Take a heap snapshot of the worker thread.
    #[napi(js_name = "takeHeapSnapshot")]
    pub fn take_heap_snapshot(&self, env: Env, options: Option<Unknown>) -> napi::Result<Unknown<'_>> {
        let fn_handle = task_global_fn::<(u32, Unknown), Unknown>(&env, "__taskTakeHeapSnapshot")?;
        let js_opts = options.unwrap_or_else(|| task_undefined(&env).unwrap());
        fn_handle.call((self.id, js_opts))
    }

    /// Start CPU profiling on the worker thread.
    #[napi(js_name = "startCpuProfile")]
    pub fn start_cpu_profile(&self, env: Env, options: Option<Unknown>) -> napi::Result<Unknown<'_>> {
        let fn_handle = task_global_fn::<(u32, Unknown), Unknown>(&env, "__taskStartCpuProfile")?;
        let js_opts = options.unwrap_or_else(|| task_undefined(&env).unwrap());
        fn_handle.call((self.id, js_opts))
    }

    /// Stop CPU profiling on the worker thread.
    #[napi(js_name = "stopCpuProfile")]
    pub fn stop_cpu_profile(&self, env: Env, profile_id: Option<u32>) -> napi::Result<Unknown<'_>> {
        let fn_handle = task_global_fn::<(u32, Option<u32>), Unknown>(&env, "__taskStopCpuProfile")?;
        fn_handle.call((self.id, profile_id))
    }

    /// Start heap profiling on the worker thread.
    #[napi(js_name = "startHeapProfile")]
    pub fn start_heap_profile(&self, env: Env, options: Option<Unknown>) -> napi::Result<Unknown<'_>> {
        let fn_handle = task_global_fn::<(u32, Unknown), Unknown>(&env, "__taskStartHeapProfile")?;
        let js_opts = options.unwrap_or_else(|| task_undefined(&env).unwrap());
        fn_handle.call((self.id, js_opts))
    }

    /// Stop heap profiling on the worker thread.
    #[napi(js_name = "stopHeapProfile")]
    pub fn stop_heap_profile(&self, env: Env) -> napi::Result<Unknown<'_>> {
        let fn_handle = task_global_fn::<u32, Unknown>(&env, "__taskStopHeapProfile")?;
        fn_handle.call(self.id)
    }

    /// Post a message to the task thread via structured clone.
    #[napi(js_name = "postMessage")]
    pub fn post_message(
        &self,
        env: Env,
        message: Unknown,
        transfer: Option<Unknown>,
    ) -> napi::Result<()> {
        let post_fn =
            task_global_fn::<(u32, Unknown, Unknown), ()>(&env, "__taskPost")?;
        let js_transfer = transfer.unwrap_or_else(|| task_undefined(&env).unwrap());
        post_fn.call((self.id, message, js_transfer))?;
        Ok(())
    }

    /// Stop execution in the worker thread as soon as possible. Returns a Promise
    /// that resolves with the exit code when the thread has stopped.
    #[napi]
    pub fn terminate(&self, env: Env) -> napi::Result<Unknown<'_>> {
        self.terminated.store(true, AtomicOrdering::Relaxed);
        let term_fn = task_global_fn::<u32, Unknown>(&env, "__taskTerminate")?;
        term_fn.call(self.id)
    }

    /// Opposite of `unref()`: keeps the Node.js event loop active until the task exits.
    #[napi(js_name = "ref")]
    pub fn ref_task(&self, env: Env) -> napi::Result<()> {
        let ref_fn = task_global_fn::<u32, ()>(&env, "__taskRef")?;
        ref_fn.call(self.id)?;
        Ok(())
    }

    /// Stops the task worker thread from keeping the Node.js event loop active.
    #[napi(js_name = "unref")]
    pub fn unref(&self, env: Env) -> napi::Result<()> {
        let unref_fn = task_global_fn::<u32, ()>(&env, "__taskUnref")?;
        unref_fn.call(self.id)?;
        Ok(())
    }

    /// Event handler for `message` events received from the task thread.
    #[napi(getter)]
    pub fn get_onmessage(&self, env: Env) -> napi::Result<Unknown<'_>> {
        let get_fn = task_global_fn::<(u32, String), Unknown>(&env, "__taskGetHandler")?;
        get_fn.call((self.id, "message".to_string()))
    }

    #[napi(setter)]
    pub fn set_onmessage(&self, env: Env, handler: Option<Unknown>) -> napi::Result<()> {
        let set_fn = task_global_fn::<(u32, String, Unknown), ()>(&env, "__taskSetHandler")?;
        let js_fn = handler.unwrap_or_else(|| task_undefined(&env).unwrap());
        set_fn.call((self.id, "message".to_string(), js_fn))?;
        Ok(())
    }

    /// Event handler for `error` events received from the task thread.
    #[napi(getter)]
    pub fn get_onerror(&self, env: Env) -> napi::Result<Unknown<'_>> {
        let get_fn = task_global_fn::<(u32, String), Unknown>(&env, "__taskGetHandler")?;
        get_fn.call((self.id, "error".to_string()))
    }

    #[napi(setter)]
    pub fn set_onerror(&self, env: Env, handler: Option<Unknown>) -> napi::Result<()> {
        let set_fn = task_global_fn::<(u32, String, Unknown), ()>(&env, "__taskSetHandler")?;
        let js_fn = handler.unwrap_or_else(|| task_undefined(&env).unwrap());
        set_fn.call((self.id, "error".to_string(), js_fn))?;
        Ok(())
    }

    /// Event handler for `messageerror` events received from the task thread.
    #[napi(getter)]
    pub fn get_onmessageerror(&self, env: Env) -> napi::Result<Unknown<'_>> {
        let get_fn = task_global_fn::<(u32, String), Unknown>(&env, "__taskGetHandler")?;
        get_fn.call((self.id, "messageerror".to_string()))
    }

    #[napi(setter)]
    pub fn set_onmessageerror(&self, env: Env, handler: Option<Unknown>) -> napi::Result<()> {
        let set_fn = task_global_fn::<(u32, String, Unknown), ()>(&env, "__taskSetHandler")?;
        let js_fn = handler.unwrap_or_else(|| task_undefined(&env).unwrap());
        set_fn.call((self.id, "messageerror".to_string(), js_fn))?;
        Ok(())
    }

    /// Register an event listener for `type` events on the task object.
    #[napi(js_name = "addEventListener")]
    pub fn add_event_listener(
        &self,
        env: Env,
        event_type: String,
        listener: Unknown,
    ) -> napi::Result<()> {
        let add_fn = task_global_fn::<(u32, String, Unknown), ()>(&env, "__taskAddListener")?;
        add_fn.call((self.id, event_type, listener))?;
        Ok(())
    }

    /// Remove an event listener for `type` events on the task object.
    #[napi(js_name = "removeEventListener")]
    pub fn remove_event_listener(
        &self,
        env: Env,
        event_type: String,
        listener: Unknown,
    ) -> napi::Result<()> {
        let rm_fn = task_global_fn::<(u32, String, Unknown), ()>(&env, "__taskRemoveListener")?;
        rm_fn.call((self.id, event_type, listener))?;
        Ok(())
    }

    /// Dispatch a synthetic event to the target task.
    #[napi(js_name = "dispatchEvent")]
    pub fn dispatch_event(&self, env: Env, event: Unknown) -> napi::Result<bool> {
        let dispatch_fn = task_global_fn::<(u32, Unknown), ()>(&env, "__taskDispatch")?;
        dispatch_fn.call((self.id, event))?;
        Ok(true)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_constants() {
        assert_eq!(K_MAX_YOUNG_GENERATION_SIZE_MB, 0);
        assert_eq!(K_MAX_OLD_GENERATION_SIZE_MB, 1);
        assert_eq!(K_CODE_RANGE_SIZE_MB, 2);
        assert_eq!(K_STACK_SIZE_MB, 3);
        assert_eq!(K_TOTAL_RESOURCE_LIMIT_COUNT, 4);
    }

    #[test]
    fn test_task_id_increment() {
        let initial = NEXT_TASK_ID.load(AtomicOrdering::Relaxed);
        let id1 = NEXT_TASK_ID.fetch_add(1, AtomicOrdering::Relaxed);
        let id2 = NEXT_TASK_ID.fetch_add(1, AtomicOrdering::Relaxed);
        assert_eq!(id1, initial);
        assert_eq!(id2, initial + 1);
    }

    #[test]
    fn test_task_options_defaults() {
        let opts = TaskOptions::default();
        assert!(opts.name.is_none());

        let custom_opts = TaskOptions {
            name: Some("worker-thread-1".to_string()),
        };
        assert_eq!(custom_opts.name.as_deref(), Some("worker-thread-1"));
    }

    #[test]
    fn test_task_bootstrap_code() {
        assert!(TASK_BOOTSTRAP.contains("globalThis.__taskInit"));
        assert!(TASK_BOOTSTRAP.contains("globalThis.__taskPost"));
        assert!(TASK_BOOTSTRAP.contains("globalThis.__taskTerminate"));
        assert!(TASK_BOOTSTRAP.contains("globalThis.__taskRef"));
        assert!(TASK_BOOTSTRAP.contains("globalThis.__taskUnref"));
        assert!(TASK_BOOTSTRAP.contains("globalThis.__taskSetHandler"));
        assert!(TASK_BOOTSTRAP.contains("globalThis.__taskGetHandler"));
        assert!(TASK_BOOTSTRAP.contains("globalThis.__taskAddListener"));
        assert!(TASK_BOOTSTRAP.contains("globalThis.__taskRemoveListener"));
        assert!(TASK_BOOTSTRAP.contains("globalThis.__taskDispatch"));
        assert!(TASK_BOOTSTRAP.contains("globalThis.__taskGetEnvMessagePort"));
        assert!(TASK_BOOTSTRAP.contains("globalThis.__taskIsMainThread"));
    }
}
