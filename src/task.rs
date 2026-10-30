use std::cell::Cell;
use std::sync::atomic::{AtomicU32, Ordering};

use napi::bindgen_prelude::{FromNapiValue, Function, ToNapiValue};
use napi::{Env, Unknown, ValueType};
use napi_derive::napi;

/// Resource limits forwarded to the underlying `node:worker_threads` `Worker`.
///
/// Mirrors the C++ `Task` `resource_limits_` float array (`node_worker.h`):
/// `maxYoungGenerationSizeMb`, `maxOldGenerationSizeMb`, `codeRangeSizeMb`,
/// `stackSizeMb`.
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

/// Index constant for `maxYoungGenerationSizeMb` resource limit.
#[napi]
pub const K_MAX_YOUNG_GENERATION_SIZE_MB: u32 = 0;
/// Index constant for `maxOldGenerationSizeMb` resource limit.
#[napi]
pub const K_MAX_OLD_GENERATION_SIZE_MB: u32 = 1;
/// Index constant for `codeRangeSizeMb` resource limit.
#[napi]
pub const K_CODE_RANGE_SIZE_MB: u32 = 2;
/// Index constant for `stackSizeMb` resource limit.
#[napi]
pub const K_STACK_SIZE_MB: u32 = 3;
/// Total count of resource limit indices.
#[napi]
pub const K_TOTAL_RESOURCE_LIMIT_COUNT: u32 = 4;

/// Options for constructing a threaded [`Task`] (a `node:worker_threads` worker).
#[napi(object)]
pub struct TaskOptions {
    /// Optional name for the worker thread.
    pub name: Option<String>,
    /// Whether the script parameter is evaluated as a string of JavaScript code.
    pub eval: Option<bool>,
    /// V8 resource limits (heap / code-range / stack size).
    #[napi(js_name = "resourceLimits")]
    pub resource_limits: Option<TaskResourceLimits>,
    /// Per-worker `process.env`. Accepts `'inherit'`, `'none'`, or a plain object.
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
    /// List of transferable objects passed during initialization.
    #[napi(js_name = "transferList")]
    pub transfer_list: Option<Vec<Unknown<'static>>>,
}

/// A threaded task: a thin Rust wrapper around a Node.js worker thread
/// (`node:worker_threads`) running a named script off the main (UI) thread.
///
/// This mirrors the Web Worker / `Worker` / `Task` model: construct it with a
/// script path, send data with [`Task::post_message`], receive results via the
/// `onmessage` handler, and tear it down with [`Task::terminate`]. Messages are
/// structured-cloned between the main thread and the task thread.
///
/// ```ts
/// const myTask = new Task("task.js", { name: "build" });
/// myTask.onmessage = (e) => console.log(e.data);
/// myTask.postMessage([1, 2, 3]);
/// ```
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

/// Bootstrap JavaScript installed once per process.
const TASK_BOOTSTRAP: &str = r#"
(function () {
  if (typeof globalThis.__taskInit === 'function') return;

  function getBuiltin(name) {
    if (typeof globalThis.__require === 'function') {
      try { return globalThis.__require(name); } catch (e) {}
    }
    if (typeof process === 'object' && process && typeof process.getBuiltinModule === 'function') {
      try { return process.getBuiltinModule(name); } catch (e) {}
    }
    if (typeof require === 'function') {
      try { return require(name); } catch (e) {}
    }
    try {
      const mod = (typeof process === 'object' && process && typeof process.getBuiltinModule === 'function')
        ? process.getBuiltinModule('node:module') : null;
      if (mod && typeof mod.createRequire === 'function') {
        return mod.createRequire(process.cwd() + '/')(name);
      }
    } catch (e) {}
    return null;
  }

  const wt = getBuiltin('node:worker_threads');

  try {
    const mod = getBuiltin('node:module');
    if (mod && mod._cache) {
      mod._cache['node:task_threads'] = { exports: wt, loaded: true };
      mod._cache['task_threads'] = { exports: wt, loaded: true };
    }
  } catch (e) {}

  const workers = (globalThis.__taskWorkers = {});
  const handlers = (globalThis.__taskHandlers = {});

  function fire(id, type, makeEvent) {
    const h = handlers[id] || {};
    const evt = makeEvent();
    if (typeof h[type] === 'function') {
      try { h[type](evt); } catch (e) {}
    }
    const list = (h.listeners && h.listeners[type]) || [];
    for (const l of list.slice()) {
      try {
        if (typeof l === 'function') {
          l(evt);
        } else if (l && typeof l.handleEvent === 'function') {
          l.handleEvent(evt);
        }
      } catch (e) {}
    }
  }

  globalThis.__taskInit = function (a) {
    const [id, path, options] = a;
    const o = options || {};
    const w = new wt.Worker(path, {
      name: o.name ?? undefined,
      eval: o.eval ?? undefined,
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
      transferList: o.transferList ?? undefined,
    });
    workers[id] = w;
    w.on('message', (d) => fire(id, 'message', () => ({ type: 'message', data: d })));
    w.on('error', (e) => fire(id, 'error', () => ({ type: 'error', error: e, message: e && e.message })));
    w.on('messageerror', (e) => fire(id, 'messageerror', () => ({ type: 'messageerror', error: e })));
    w.on('exit', (code, sig) => fire(id, 'exit', () => ({ type: 'exit', code, signal: sig })));
    return w.threadId;
  };

  globalThis.__taskPost = function (a) {
    const [id, msg, transfer] = a;
    const w = workers[id]; if (!w) return;
    if (transfer !== undefined) {
      w.postMessage(msg, transfer);
    } else {
      w.postMessage(msg);
    }
  };

  globalThis.__taskTerminate = function (a) {
    const [id] = a;
    const w = workers[id]; if (!w) return Promise.resolve(0);
    delete workers[id];
    delete handlers[id];
    return w.terminate();
  };

  globalThis.__taskRef = function (a) {
    const [id] = a; const w = workers[id]; if (w && w.ref) w.ref();
  };

  globalThis.__taskUnref = function (a) {
    const [id] = a; const w = workers[id]; if (w && w.unref) w.unref();
  };

  globalThis.__taskSetHandler = function (a) {
    const [id, type, fn] = a;
    const h = handlers[id] || (handlers[id] = { listeners: {} });
    h[type] = fn;
  };

  globalThis.__taskGetHandler = function (a) {
    const [id, type] = a;
    const h = handlers[id];
    return h ? h[type] : undefined;
  };

  globalThis.__taskAddListener = function (a) {
    const [id, type, fn, opts] = a;
    if (!fn) return;
    const h = handlers[id] || (handlers[id] = { listeners: {} });
    const list = (h.listeners[type] || (h.listeners[type] = []));
    let wrapper = fn;
    if (opts && typeof opts === 'object' && opts.once) {
      wrapper = function (evt) {
        globalThis.__taskRemoveListener([id, type, fn]);
        if (typeof fn === 'function') fn(evt);
        else if (fn && typeof fn.handleEvent === 'function') fn.handleEvent(evt);
      };
      wrapper._orig = fn;
    }
    if (opts && typeof opts === 'object' && opts.signal) {
      if (opts.signal.aborted) return;
      opts.signal.addEventListener('abort', () => {
        globalThis.__taskRemoveListener([id, type, fn]);
      }, { once: true });
    }
    if (!list.some((item) => item === fn || item._orig === fn)) {
      list.push(wrapper);
    }
  };

  globalThis.__taskRemoveListener = function (a) {
    const [id, type, fn] = a;
    const h = handlers[id]; if (!h || !h.listeners || !h.listeners[type]) return;
    h.listeners[type] = h.listeners[type].filter((l) => l !== fn && l._orig !== fn);
  };

  globalThis.__taskDispatch = function (a) {
    const [id, evt] = a;
    const h = handlers[id] || {};
    const t = evt && evt.type; if (!t) return true;
    let canceled = false;
    if (evt && typeof evt.preventDefault === 'function') {
      const orig = evt.preventDefault;
      evt.preventDefault = function () { canceled = true; orig.call(evt); };
    }
    if (typeof h[t] === 'function') { try { h[t](evt); } catch (e) {} }
    const list = (h.listeners && h.listeners[t]) || [];
    for (const l of list.slice()) {
      try {
        if (typeof l === 'function') {
          l(evt);
        } else if (l && typeof l.handleEvent === 'function') {
          l.handleEvent(evt);
        }
      } catch (e) {}
    }
    return !canceled;
  };

  globalThis.__taskGetHeapSnapshot = function (a) {
    const [id, opts] = a; const w = workers[id]; if (!w) return undefined;
    return w.getHeapSnapshot(opts);
  };

  globalThis.__taskGetHeapStatistics = function (a) {
    const [id] = a; const w = workers[id]; if (!w) return undefined;
    return w.getHeapStatistics();
  };

  globalThis.__taskCpuUsage = function (a) {
    const [id, prev] = a; const w = workers[id]; if (!w) return undefined;
    return w.cpuUsage(prev);
  };

  globalThis.__taskEventLoopUtilization = function (a) {
    const [id, prev] = a; const w = workers[id]; if (!w) return undefined;
    return w.performance.eventLoopUtilization(prev);
  };

  globalThis.__taskStartCpuProfile = function (a) {
    const [id, opts] = a; const w = workers[id]; if (!w) return undefined;
    return w.startCpuProfile(opts);
  };

  globalThis.__taskStopCpuProfile = function (a) {
    const [id, opts] = a; const w = workers[id]; if (!w) return undefined;
    return w.stopCpuProfile(opts);
  };

  globalThis.__taskStartHeapProfile = function (a) {
    const [id, opts] = a; const w = workers[id]; if (!w) return undefined;
    return w.startHeapProfile(opts);
  };

  globalThis.__taskStopHeapProfile = function (a) {
    const [id, opts] = a; const w = workers[id]; if (!w) return undefined;
    return w.stopHeapProfile(opts);
  };
})();
"#;

fn task_ensure_bootstrap(env: &Env) -> napi::Result<()> {
    let _: Unknown<'_> = env.run_script::<_, Unknown<'_>>(TASK_BOOTSTRAP)?;
    Ok(())
}

fn to_unknown<T: ToNapiValue>(env: &Env, val: T) -> napi::Result<Unknown<'static>> {
    let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), val)? };
    Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), raw) })
}

fn undefined_value(env: &Env) -> napi::Result<Unknown<'static>> {
    let u: Unknown<'_> = env.run_script::<_, Unknown<'_>>("undefined")?;
    let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), u)? };
    Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), raw) })
}

fn args_array(env: &Env, items: &[Unknown<'static>]) -> napi::Result<Unknown<'static>> {
    let mut arr = env.create_array(items.len() as u32)?;
    for (i, it) in items.iter().copied().enumerate() {
        arr.set(i as u32, it)?;
    }
    to_unknown(env, arr)
}

fn task_call(env: &Env, name: &str, args: Unknown<'static>) -> napi::Result<Unknown<'static>> {
    let func: Function<'_> = env.run_script::<_, Function<'_>>(format!("globalThis.{name}"))?;
    let result: Unknown<'_> = func.call(args)?;
    let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), result)? };
    Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), raw) })
}

/// Returns whether the current thread is the main thread.
#[napi]
pub fn is_main_thread(env: Env) -> napi::Result<bool> {
    let func: Function<'_> = env.run_script::<_, Function<'_>>(
        "(function() { try { const wt = (typeof process === 'object' && process && typeof process.getBuiltinModule === 'function') ? process.getBuiltinModule('node:worker_threads') : (typeof globalThis.__require === 'function' ? globalThis.__require('node:worker_threads') : (typeof require === 'function' ? require('node:worker_threads') : null)); return wt ? wt.isMainThread : true; } catch(e) { return true; } })",
    )?;
    let undef = undefined_value(&env)?;
    let result: Unknown<'_> = func.call(undef)?;
    let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), result)? };
    unsafe { bool::from_napi_value(env.raw(), raw) }
}

/// Returns the MessagePort communicating with the parent thread inside a Task worker thread.
#[napi]
pub fn get_env_message_port(env: Env) -> napi::Result<Option<Unknown<'static>>> {
    let func: Function<'_> = env.run_script::<_, Function<'_>>(
        "(function() { try { const wt = (typeof process === 'object' && process && typeof process.getBuiltinModule === 'function') ? process.getBuiltinModule('node:worker_threads') : (typeof globalThis.__require === 'function' ? globalThis.__require('node:worker_threads') : (typeof require === 'function' ? require('node:worker_threads') : null)); return wt ? wt.parentPort : null; } catch(e) { return null; } })",
    )?;
    let undef = undefined_value(&env)?;
    let result: Unknown<'_> = func.call(undef)?;
    let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), result)? };
    Ok(Some(unsafe { Unknown::from_raw_unchecked(env.raw(), raw) }))
}

#[napi]
impl Task {
    /// Create a task that runs `script` on a dedicated worker thread.
    #[napi(constructor)]
    pub fn new(env: Env, script: String, options: Option<TaskOptions>) -> napi::Result<Self> {
        task_ensure_bootstrap(&env)?;
        let id = NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed);

        let opts_unknown = match options {
            Some(o) => to_unknown(&env, o)?,
            None => undefined_value(&env)?,
        };
        let script_unknown = to_unknown(&env, env.create_string(&script)?)?;
        let id_unknown = to_unknown(&env, env.create_uint32(id)?)?;
        let args = args_array(&env, &[id_unknown, script_unknown, opts_unknown])?;

        let func: Function<'_> = env.run_script::<_, Function<'_>>("globalThis.__taskInit")?;
        let result: Unknown<'_> = func.call(args)?;
        let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), result)? };
        let thread_id = unsafe { u32::from_napi_value(env.raw(), raw)? };

        Ok(Task {
            id,
            thread_id,
            terminated: Cell::new(false),
        })
    }

    /// The `Worker.threadId` assigned to this task (available immediately after construction).
    #[napi(getter, js_name = "threadId")]
    pub fn get_thread_id(&self) -> u32 {
        self.thread_id
    }

    /// Whether [`Task::terminate`] has been called.
    #[napi(getter)]
    pub fn get_terminated(&self) -> bool {
        self.terminated.get()
    }

    /// Send `message` to the worker thread.
    #[napi(js_name = "postMessage")]
    pub fn post_message(
        &self,
        env: Env,
        message: Unknown<'static>,
        transfer_or_options: Option<Unknown<'static>>,
    ) -> napi::Result<()> {
        let id_unknown = to_unknown(&env, env.create_uint32(self.id)?)?;
        let transfer_unknown = match transfer_or_options {
            Some(t) => t,
            None => undefined_value(&env)?,
        };
        let args = args_array(&env, &[id_unknown, message, transfer_unknown])?;
        task_call(&env, "__taskPost", args)?;
        Ok(())
    }

    /// Terminate the worker thread.
    #[napi(ts_return_type = "Promise<number>")]
    pub fn terminate(&self, env: Env) -> napi::Result<Unknown<'static>> {
        let id_unknown = to_unknown(&env, env.create_uint32(self.id)?)?;
        let args = args_array(&env, &[id_unknown])?;
        self.terminated.set(true);
        task_call(&env, "__taskTerminate", args)
    }

    /// Increment the worker's reference count.
    #[napi(js_name = "ref")]
    pub fn ref_(&self, env: Env) -> napi::Result<()> {
        self.reference(env)
    }

    /// Decrement the worker's reference count.
    #[napi(js_name = "unref")]
    pub fn unref_(&self, env: Env) -> napi::Result<()> {
        self.unreference(env)
    }

    /// Increment the worker's reference count (alias for `ref`).
    #[napi]
    pub fn reference(&self, env: Env) -> napi::Result<()> {
        let id_unknown = to_unknown(&env, env.create_uint32(self.id)?)?;
        let args = args_array(&env, &[id_unknown])?;
        task_call(&env, "__taskRef", args)?;
        Ok(())
    }

    /// Decrement the worker's reference count (alias for `unref`).
    #[napi]
    pub fn unreference(&self, env: Env) -> napi::Result<()> {
        let id_unknown = to_unknown(&env, env.create_uint32(self.id)?)?;
        let args = args_array(&env, &[id_unknown])?;
        task_call(&env, "__taskUnref", args)?;
        Ok(())
    }

    #[napi(getter)]
    pub fn get_onmessage(&self, env: Env) -> napi::Result<Option<Function<'static>>> {
        task_get_handler(&env, self.id, "message")
    }

    #[napi(setter)]
    pub fn set_onmessage(&self, env: Env, handler: Option<Function<'static>>) -> napi::Result<()> {
        task_set_handler(&env, self.id, "message", handler)
    }

    #[napi(getter)]
    pub fn get_onerror(&self, env: Env) -> napi::Result<Option<Function<'static>>> {
        task_get_handler(&env, self.id, "error")
    }

    #[napi(setter)]
    pub fn set_onerror(&self, env: Env, handler: Option<Function<'static>>) -> napi::Result<()> {
        task_set_handler(&env, self.id, "error", handler)
    }

    #[napi(getter)]
    pub fn get_onmessageerror(&self, env: Env) -> napi::Result<Option<Function<'static>>> {
        task_get_handler(&env, self.id, "messageerror")
    }

    #[napi(setter)]
    pub fn set_onmessageerror(
        &self,
        env: Env,
        handler: Option<Function<'static>>,
    ) -> napi::Result<()> {
        task_set_handler(&env, self.id, "messageerror", handler)
    }

    /// Register an event listener (mirrors `EventTarget.addEventListener`).
    #[napi(js_name = "addEventListener")]
    pub fn add_event_listener(
        &self,
        env: Env,
        type_: String,
        listener: Unknown<'static>,
        options: Option<Unknown<'static>>,
    ) -> napi::Result<()> {
        let id_unknown = to_unknown(&env, env.create_uint32(self.id)?)?;
        let type_unknown = to_unknown(&env, env.create_string(&type_)?)?;
        let options_unknown = match options {
            Some(o) => o,
            None => undefined_value(&env)?,
        };
        let args = args_array(&env, &[id_unknown, type_unknown, listener, options_unknown])?;
        task_call(&env, "__taskAddListener", args)?;
        Ok(())
    }

    /// Remove a previously registered event listener.
    #[napi(js_name = "removeEventListener")]
    pub fn remove_event_listener(
        &self,
        env: Env,
        type_: String,
        listener: Unknown<'static>,
        options: Option<Unknown<'static>>,
    ) -> napi::Result<()> {
        let id_unknown = to_unknown(&env, env.create_uint32(self.id)?)?;
        let type_unknown = to_unknown(&env, env.create_string(&type_)?)?;
        let options_unknown = match options {
            Some(o) => o,
            None => undefined_value(&env)?,
        };
        let args = args_array(&env, &[id_unknown, type_unknown, listener, options_unknown])?;
        task_call(&env, "__taskRemoveListener", args)?;
        Ok(())
    }

    /// Dispatch an event to this task's listeners (mirrors `EventTarget.dispatchEvent`).
    #[napi(js_name = "dispatchEvent")]
    pub fn dispatch_event(&self, env: Env, evt: Unknown<'static>) -> napi::Result<bool> {
        let id_unknown = to_unknown(&env, env.create_uint32(self.id)?)?;
        let args = args_array(&env, &[id_unknown, evt])?;
        let result = task_call(&env, "__taskDispatch", args)?;
        let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), result)? };
        unsafe { bool::from_napi_value(env.raw(), raw) }
    }

    /// Capture a heap snapshot of the worker, returned as a Node `Readable` stream.
    #[napi(ts_return_type = "Promise<import('stream').Readable>")]
    pub fn get_heap_snapshot(
        &self,
        env: Env,
        options: Option<Unknown<'static>>,
    ) -> napi::Result<Unknown<'static>> {
        task_diag(&env, self.id, "__taskGetHeapSnapshot", options)
    }

    /// Return the worker's V8 heap statistics.
    #[napi(ts_return_type = "Promise<object>")]
    pub fn get_heap_statistics(&self, env: Env) -> napi::Result<Unknown<'static>> {
        let id_unknown = to_unknown(&env, env.create_uint32(self.id)?)?;
        let args = args_array(&env, &[id_unknown])?;
        task_call(&env, "__taskGetHeapStatistics", args)
    }

    /// CPU time used by the worker so far (`{ user, system }` in microseconds).
    #[napi(ts_return_type = "{ user: number, system: number }")]
    pub fn cpu_usage(
        &self,
        env: Env,
        prev: Option<Unknown<'static>>,
    ) -> napi::Result<Unknown<'static>> {
        task_diag(&env, self.id, "__taskCpuUsage", prev)
    }

    /// Event-loop utilization of the worker.
    #[napi(ts_return_type = "object")]
    pub fn event_loop_utilization(
        &self,
        env: Env,
        prev: Option<Unknown<'static>>,
    ) -> napi::Result<Unknown<'static>> {
        task_diag(&env, self.id, "__taskEventLoopUtilization", prev)
    }

    /// Start a CPU profile of the worker.
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

    /// Start a heap profile of the worker.
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

fn task_set_handler(
    env: &Env,
    id: u32,
    type_: &str,
    handler: Option<Function<'static>>,
) -> napi::Result<()> {
    let id_unknown = to_unknown(env, env.create_uint32(id)?)?;
    let type_unknown = to_unknown(env, env.create_string(type_)?)?;
    let handler_unknown = match handler {
        Some(h) => to_unknown(env, h)?,
        None => undefined_value(env)?,
    };
    let args = args_array(env, &[id_unknown, type_unknown, handler_unknown])?;
    task_call(env, "__taskSetHandler", args)?;
    Ok(())
}

fn task_get_handler(env: &Env, id: u32, type_: &str) -> napi::Result<Option<Function<'static>>> {
    let id_unknown = to_unknown(env, env.create_uint32(id)?)?;
    let type_unknown = to_unknown(env, env.create_string(type_)?)?;
    let args = args_array(env, &[id_unknown, type_unknown])?;
    let result = task_call(env, "__taskGetHandler", args)?;
    match result.get_type()? {
        ValueType::Undefined | ValueType::Null => Ok(None),
        _ => {
            let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), result)? };
            Ok(Some(unsafe { Function::from_napi_value(env.raw(), raw)? }))
        }
    }
}

fn task_diag(
    env: &Env,
    id: u32,
    name: &str,
    options: Option<Unknown<'static>>,
) -> napi::Result<Unknown<'static>> {
    let id_unknown = to_unknown(env, env.create_uint32(id)?)?;
    let opts_unknown = match options {
        Some(o) => o,
        None => undefined_value(env)?,
    };
    let args = args_array(env, &[id_unknown, opts_unknown])?;
    task_call(env, name, args)
}
