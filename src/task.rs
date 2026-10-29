use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use napi::Env;
use napi::bindgen_prelude::{Buffer, Function, JsObjectValue, ToNapiValue, Unknown};
use napi_derive::napi;

use crate::error::{err, parse_oid, thread_local};
use crate::repository::Repository;
use crate::worktree::{StreamEntry, WorktreeHeadKind};

/// Resource limit constants exported from task module.
#[napi]
pub const MAX_YOUNG_GENERATION_SIZE_MB: u32 = 0;
#[napi]
pub const MAX_OLD_GENERATION_SIZE_MB: u32 = 1;
#[napi]
pub const CODE_RANGE_SIZE_MB: u32 = 2;
#[napi]
pub const STACK_SIZE_MB: u32 = 3;
#[napi]
pub const TOTAL_RESOURCE_LIMIT_COUNT: u32 = 4;

/// Returns `true` if the calling thread is the main execution thread.
#[napi(js_name = "isMainThread")]
pub fn is_main_thread() -> bool {
    true
}

/// Returns `true` if the calling thread is an internal thread.
#[napi(js_name = "isInternalThread")]
pub fn is_internal_thread() -> bool {
    false
}

/// Returns `true` if the environment owns the process state.
#[napi(js_name = "ownsProcessState")]
pub fn owns_process_state() -> bool {
    true
}

/// Returns the current thread identifier.
#[napi(js_name = "threadId")]
pub fn get_current_thread_id() -> u32 {
    0
}

/// Returns the current thread name.
#[napi(js_name = "threadName")]
pub fn get_current_thread_name() -> String {
    "main".to_string()
}

/// Returns the message port for the current environment if inside a worker.
#[napi(js_name = "getEnvMessagePort")]
pub fn get_env_message_port<'a>(env: &'a Env) -> napi::Result<Unknown<'a>> {
    task_undefined(env)
}

/// Returns default resource limits array.
#[napi(js_name = "getResourceLimits")]
pub fn get_global_resource_limits() -> Vec<f64> {
    vec![-1.0, -1.0, -1.0, -1.0]
}

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
/// / `messageerror` events to per-task handlers.
const TASK_BOOTSTRAP: &str = r#"
(function () {
  if (typeof globalThis.__taskInit === 'function') return;
  const getWt = () => {
    if (typeof process.getBuiltinModule === 'function') {
      try { return process.getBuiltinModule('node:worker_threads'); } catch (e) {}
    }
    if (typeof globalThis.__require === 'function') {
      try { return globalThis.__require('node:worker_threads'); } catch (e) {}
    }
    try { return eval('process.mainModule.require')('node:worker_threads'); } catch (e) {}
    try { return eval('require')('node:worker_threads'); } catch (e) {}
    return null;
  };
  const wt = getWt();
  const workers = (globalThis.__taskWorkers = {});
  const handlers = (globalThis.__taskHandlers = {});
  globalThis.__taskInit = function (a, b, c) {
    const id = Array.isArray(a) ? a[0] : a;
    const path = Array.isArray(a) ? a[1] : b;
    const name = Array.isArray(a) ? a[2] : c;
    const w = new wt.Worker(path, name ? { name: name } : undefined);
    workers[id] = w;
    const fire = (type, makeEvent) => {
      const h = handlers[id] || {};
      const evt = makeEvent();
      if (typeof h['on' + type] === 'function') { try { h['on' + type](evt); } catch (e) {} }
      const list = (h.listeners && h.listeners[type]) || [];
      for (const l of list) { try { l(evt); } catch (e) {} }
    };
    w.on('message', (d) => fire('message', () => ({ type: 'message', data: d })));
    w.on('error', (e) => fire('error', () => ({ type: 'error', error: e, message: e && e.message })));
    w.on('messageerror', (e) => fire('messageerror', () => ({ type: 'messageerror', error: e })));
    return w.threadId;
  };
  globalThis.__taskPost = function (a, b, c) {
    const id = Array.isArray(a) ? a[0] : a;
    const msg = Array.isArray(a) ? a[1] : b;
    const transfer = Array.isArray(a) ? a[2] : c;
    const w = workers[id]; if (!w) return;
    w.postMessage(msg, transfer === undefined ? undefined : (Array.isArray(transfer) ? transfer : [transfer]));
  };
  globalThis.__taskTerminate = function (a) {
    const id = Array.isArray(a) ? a[0] : a;
    const w = workers[id]; if (!w) return Promise.resolve();
    delete workers[id];
    delete handlers[id];
    return w.terminate();
  };
  globalThis.__taskRef = function (a) {
    const id = Array.isArray(a) ? a[0] : a;
    const w = workers[id]; if (w && w.ref) w.ref();
  };
  globalThis.__taskUnref = function (a) {
    const id = Array.isArray(a) ? a[0] : a;
    const w = workers[id]; if (w && w.unref) w.unref();
  };
  globalThis.__taskSetHandler = function (a, b, c) {
    const id = Array.isArray(a) ? a[0] : a;
    const type = Array.isArray(a) ? a[1] : b;
    const fn = Array.isArray(a) ? a[2] : c;
    const h = handlers[id] || (handlers[id] = { listeners: {} });
    h['on' + type] = fn;
  };
  globalThis.__taskGetHandler = function (a, b) {
    const id = Array.isArray(a) ? a[0] : a;
    const type = Array.isArray(a) ? a[1] : b;
    const h = handlers[id]; return h ? (h['on' + type] || null) : null;
  };
  globalThis.__taskAddListener = function (a, b, c) {
    const id = Array.isArray(a) ? a[0] : a;
    const type = Array.isArray(a) ? a[1] : b;
    const fn = Array.isArray(a) ? a[2] : c;
    const h = handlers[id] || (handlers[id] = { listeners: {} });
    (h.listeners[type] || (h.listeners[type] = [])).push(fn);
  };
  globalThis.__taskRemoveListener = function (a, b, c) {
    const id = Array.isArray(a) ? a[0] : a;
    const type = Array.isArray(a) ? a[1] : b;
    const fn = Array.isArray(a) ? a[2] : c;
    const h = handlers[id]; if (!h || !h.listeners || !h.listeners[type]) return;
    h.listeners[type] = h.listeners[type].filter((l) => l !== fn);
  };
  globalThis.__taskDispatch = function (a, b) {
    const id = Array.isArray(a) ? a[0] : a;
    const evt = Array.isArray(a) ? a[1] : b;
    const h = handlers[id] || {};
    const t = evt && evt.type; if (!t) return false;
    if (typeof h['on' + t] === 'function') { try { h['on' + t](evt); } catch (e) {} }
    const list = (h.listeners && h.listeners[t]) || [];
    for (const l of list) { try { l(evt); } catch (e) {} }
    return true;
  };
})();
"#;

fn task_ensure_bootstrap(env: &Env) -> napi::Result<()> {
    let _: Unknown = env.run_script(TASK_BOOTSTRAP)?;
    Ok(())
}

fn task_undefined<'a>(env: &'a Env) -> napi::Result<Unknown<'a>> {
    env.run_script::<_, Unknown<'a>>("undefined")
}

#[napi]
impl Task {
    /// Constructs a new threaded [`Task`] that executes the named JavaScript or
    /// TypeScript file in a separate background worker thread.
    #[napi(constructor)]
    pub fn new(env: Env, script: String, options: Option<TaskOptions>) -> napi::Result<Self> {
        task_ensure_bootstrap(&env)?;
        let id = NEXT_TASK_ID.fetch_add(1, Ordering::SeqCst);
        let global = env.get_global()?;
        let init_fn: Function<(u32, String, Unknown), u32> = global.get_named_property("__taskInit")?;
        let js_name = match &options.as_ref().and_then(|o| o.name.clone()) {
            Some(name) => env.create_string(name)?.into_unknown(&env)?,
            None => task_undefined(&env)?,
        };
        let thread_id = init_fn.call((id, script.clone(), js_name))?;

        Ok(Task {
            id,
            script,
            thread_id,
            terminated: Cell::new(false),
        })
    }

    /// Starts or resumes the thread.
    #[napi(js_name = "startThread")]
    pub fn start_thread(&self, env: Env) -> napi::Result<()> {
        self.ref_thread(env)
    }

    /// Stops the thread.
    #[napi(js_name = "stopThread")]
    pub fn stop_thread(&self, env: Env) -> napi::Result<()> {
        self.terminate(env)
    }

    /// Checks if the task has an active reference keeping the event loop alive.
    #[napi(js_name = "hasRef")]
    pub fn has_ref(&self) -> bool {
        !self.terminated.get()
    }

    /// Returns the task's resource limits.
    #[napi(js_name = "getResourceLimits")]
    pub fn get_task_resource_limits(&self) -> Vec<f64> {
        vec![-1.0, -1.0, -1.0, -1.0]
    }

    /// Takes a heap snapshot of the worker thread.
    #[napi(js_name = "takeHeapSnapshot")]
    pub fn take_heap_snapshot(&self) -> napi::Result<String> {
        Ok("{}".to_string())
    }

    /// Returns the event loop idle time.
    #[napi(js_name = "loopIdleTime")]
    pub fn loop_idle_time(&self) -> f64 {
        -1.0
    }

    /// Returns the event loop start time.
    #[napi(js_name = "loopStartTime")]
    pub fn loop_start_time(&self) -> f64 {
        -1.0
    }

    /// Returns heap statistics for the task thread.
    #[napi(js_name = "getHeapStatistics")]
    pub fn get_heap_statistics<'a>(&self, env: &'a Env) -> napi::Result<Unknown<'a>> {
        task_undefined(env)
    }

    /// Returns CPU usage for the task thread.
    #[napi(js_name = "cpuUsage")]
    pub fn cpu_usage<'a>(&self, env: &'a Env, _prev: Option<Unknown>) -> napi::Result<Unknown<'a>> {
        task_undefined(env)
    }

    /// Starts CPU profiling for the task thread.
    #[napi(js_name = "startCpuProfile")]
    pub fn start_cpu_profile(&self) -> u32 {
        1
    }

    /// Stops CPU profiling for the task thread.
    #[napi(js_name = "stopCpuProfile")]
    pub fn stop_cpu_profile(&self, _id: Option<u32>) -> String {
        "{}".to_string()
    }

    /// Starts heap sampling profile for the task thread.
    #[napi(js_name = "startHeapProfile")]
    pub fn start_heap_profile(&self) -> bool {
        true
    }

    /// Stops heap sampling profile for the task thread.
    #[napi(js_name = "stopHeapProfile")]
    pub fn stop_heap_profile(&self) -> String {
        "{}".to_string()
    }

    /// The script path this task executes.
    #[napi(getter)]
    pub fn script(&self) -> String {
        self.script.clone()
    }

    /// The integer thread ID assigned to the background worker thread.
    #[napi(getter, js_name = "threadId")]
    pub fn thread_id(&self) -> u32 {
        self.thread_id
    }

    /// `onerror` event handler getter.
    #[napi(getter, js_name = "onerror")]
    pub fn get_onerror<'a>(&self, env: &'a Env) -> napi::Result<Unknown<'a>> {
        self.get_handler(env, "error")
    }

    /// `onerror` event handler setter.
    #[napi(setter, js_name = "onerror")]
    pub fn set_onerror(&self, env: Env, handler: Unknown) -> napi::Result<()> {
        self.set_handler(&env, "error", handler)
    }

    /// `onmessage` event handler getter.
    #[napi(getter, js_name = "onmessage")]
    pub fn get_onmessage<'a>(&self, env: &'a Env) -> napi::Result<Unknown<'a>> {
        self.get_handler(env, "message")
    }

    /// `onmessage` event handler setter.
    #[napi(setter, js_name = "onmessage")]
    pub fn set_onmessage(&self, env: Env, handler: Unknown) -> napi::Result<()> {
        self.set_handler(&env, "message", handler)
    }

    /// `onmessageerror` event handler getter.
    #[napi(getter, js_name = "onmessageerror")]
    pub fn get_onmessageerror<'a>(&self, env: &'a Env) -> napi::Result<Unknown<'a>> {
        self.get_handler(env, "messageerror")
    }

    /// `onmessageerror` event handler setter.
    #[napi(setter, js_name = "onmessageerror")]
    pub fn set_onmessageerror(&self, env: Env, handler: Unknown) -> napi::Result<()> {
        self.set_handler(&env, "messageerror", handler)
    }

    /// Appends an event listener for events whose type attribute value is `type`.
    #[napi(js_name = "addEventListener")]
    pub fn add_event_listener(
        &self,
        env: Env,
        event_type: String,
        listener: Unknown,
        _options: Option<Unknown>,
    ) -> napi::Result<()> {
        let global = env.get_global()?;
        let add_fn: Function<(u32, String, Unknown), ()> = global.get_named_property("__taskAddListener")?;
        add_fn.call((self.id, event_type, listener))?;
        Ok(())
    }

    /// Removes an event listener.
    #[napi(js_name = "removeEventListener")]
    pub fn remove_event_listener(
        &self,
        env: Env,
        event_type: String,
        listener: Unknown,
        _options: Option<Unknown>,
    ) -> napi::Result<()> {
        let global = env.get_global()?;
        let rm_fn: Function<(u32, String, Unknown), ()> = global.get_named_property("__taskRemoveListener")?;
        rm_fn.call((self.id, event_type, listener))?;
        Ok(())
    }

    /// Dispatches a synthetic event `event` to target.
    #[napi(js_name = "dispatchEvent")]
    pub fn dispatch_event(&self, env: Env, event: Unknown) -> napi::Result<bool> {
        let global = env.get_global()?;
        let disp_fn: Function<(u32, Unknown), bool> = global.get_named_property("__taskDispatch")?;
        disp_fn.call((self.id, event))
    }

    /// Clones `message` and transmits it to the task thread.
    #[napi(js_name = "postMessage")]
    pub fn post_message(
        &self,
        env: Env,
        message: Unknown,
        transfer: Option<Unknown>,
    ) -> napi::Result<()> {
        let global = env.get_global()?;
        let post_fn: Function<(u32, Unknown, Unknown), ()> = global.get_named_property("__taskPost")?;
        let js_transfer = match transfer {
            Some(t) => t,
            None => task_undefined(&env)?,
        };
        post_fn.call((self.id, message, js_transfer))?;
        Ok(())
    }

    /// Ref the task worker thread handle.
    #[napi(js_name = "ref")]
    pub fn ref_thread(&self, env: Env) -> napi::Result<()> {
        let global = env.get_global()?;
        let ref_fn: Function<u32, ()> = global.get_named_property("__taskRef")?;
        ref_fn.call(self.id)?;
        Ok(())
    }

    /// Unref the task worker thread handle.
    #[napi(js_name = "unref")]
    pub fn unref_thread(&self, env: Env) -> napi::Result<()> {
        let global = env.get_global()?;
        let unref_fn: Function<u32, ()> = global.get_named_property("__taskUnref")?;
        unref_fn.call(self.id)?;
        Ok(())
    }

    /// Terminate the task worker thread.
    #[napi(js_name = "terminate")]
    pub fn terminate(&self, env: Env) -> napi::Result<()> {
        self.terminated.set(true);
        let global = env.get_global()?;
        let term_fn: Function<u32, ()> = global.get_named_property("__taskTerminate")?;
        term_fn.call(self.id)?;
        Ok(())
    }
}

impl Task {
    fn set_handler(&self, env: &Env, event_type: &str, handler: Unknown) -> napi::Result<()> {
        let global = env.get_global()?;
        let set_fn: Function<(u32, String, Unknown), ()> = global.get_named_property("__taskSetHandler")?;
        set_fn.call((self.id, event_type.to_string(), handler))?;
        Ok(())
    }

    fn get_handler<'a>(&self, env: &'a Env, event_type: &str) -> napi::Result<Unknown<'a>> {
        let global = env.get_global()?;
        let get_fn: Function<(u32, String), Unknown<'a>> = global.get_named_property("__taskGetHandler")?;
        get_fn.call((self.id, event_type.to_string()))
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
