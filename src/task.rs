use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

use napi::bindgen_prelude::{Function, Unknown};
use napi::Env;
use napi_derive::napi;

pub use crate::worktree::{AddWorktreeTask, RemoveWorktreeTask, StreamRawTask, StreamTask};

/// Options for constructing a threaded [`Task`].
#[napi(object)]
#[derive(Default)]
pub struct TaskOptions {
    /// Optional name for the worker thread (forwarded to `Worker`'s `name`
    /// option). Useful for debugging / profiling under `node --trace-warnings`.
    pub name: Option<String>,
}

/// Task types supported by the Task API.
#[napi]
pub enum TaskType {
    Dedicated,
    Shared,
    Service,
}

/// Global scope for dedicated tasks.
#[napi]
pub struct DedicatedTaskGlobalScope;

/// Global scope for shared tasks.
#[napi]
pub struct SharedTaskGlobalScope;

/// Global scope for service tasks.
#[napi]
pub struct ServiceTaskGlobalScope;

/// A threaded task: a wrapper around a Node.js worker thread
/// (`node:worker_threads`) that runs a named script in the background, separate
/// from the main (UI) thread.
///
/// This mirrors the Web Worker / `Worker` model: construct it with a script
/// path, send data with [`Task::postMessage`], receive results via the
/// `onmessage` handler, and tear it down with [`Task::terminate`].
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
    /// Optional thread name.
    name: Option<String>,
    /// Whether [`Task::terminate`] has been called.
    terminated: bool,
}

/// Monotonic, process-unique id for each [`Task`] instance.
static NEXT_TASK_ID: AtomicU32 = AtomicU32::new(1);

/// Bootstrap JavaScript installed once per process. It defines global helpers
/// that create/own the `Worker` instances and forward their `message` / `error`
/// / `messageerror` events to per-task handlers.
const TASK_BOOTSTRAP: &str = r#"
(function () {
  if (typeof globalThis.__taskInit === 'function') return;
  let wt;
  try {
    if (typeof process !== 'undefined' && typeof process.getBuiltinModule === 'function') {
      wt = process.getBuiltinModule('node:worker_threads');
    }
  } catch (e) {}
  if (!wt) {
    try {
      if (typeof globalThis.__require === 'function') {
        wt = globalThis.__require('node:worker_threads');
      } else if (typeof require === 'function') {
        wt = require('node:worker_threads');
      } else {
        wt = eval("require('node:worker_threads')");
      }
    } catch (e) {
      try { wt = eval("require('worker_threads')"); } catch (e2) {}
    }
  }
  const workers = (globalThis.__taskWorkers = {});
  const handlers = (globalThis.__taskHandlers = {});
  const envData = (globalThis.__taskEnvData = new Map());
  const threadPorts = (globalThis.__taskThreadPorts = new Map());

  globalThis.__taskInit = function (a, b, c) {
    let id = a, path = b, name = c;
    if (Array.isArray(a)) { [id, path, name] = a; }
    if (!wt) return 0;
    const w = new wt.Worker(path, name ? { name: name } : undefined);
    workers[id] = w;
    const fire = (type, makeEvent) => {
      const h = handlers[id] || {};
      const evt = makeEvent();
      if (typeof h[type] === 'function') { try { h[type](evt); } catch (e) {} }
      const list = (h.listeners && h.listeners[type]) || [];
      for (const l of list) { try { l(evt); } catch (e) {} }
    };
    w.on('message', (d) => { fire('message', () => ({ data: d, type: 'message' })); });
    w.on('error', (e) => { fire('error', () => ({ error: e, message: e && e.message, type: 'error' })); });
    w.on('messageerror', (e) => fire('messageerror', () => ({ error: e, type: 'messageerror' })));
    return w.threadId;
  };

  globalThis.__taskPost = function (a, b, c) {
    let id = a, msg = b, transfer = c;
    if (Array.isArray(a) && b === undefined) { [id, msg, transfer] = a; }
    const w = workers[id]; if (!w) return;
    let tList = Array.isArray(transfer) ? transfer : (transfer && Array.isArray(transfer.transfer) ? transfer.transfer : undefined);
    w.postMessage(msg, tList);
  };

  globalThis.__taskTerminate = function (a) {
    let id = Array.isArray(a) ? a[0] : a;
    const w = workers[id]; if (!w) return Promise.resolve();
    delete workers[id];
    delete handlers[id];
    return w.terminate();
  };

  globalThis.__taskRef = function (a) {
    let id = Array.isArray(a) ? a[0] : a;
    const w = workers[id]; if (w && w.ref) w.ref();
  };

  globalThis.__taskUnref = function (a) {
    let id = Array.isArray(a) ? a[0] : a;
    const w = workers[id]; if (w && w.unref) w.unref();
  };

  globalThis.__taskSetHandler = function (a, b, c) {
    let id = a, type = b, fn = c;
    if (Array.isArray(a) && b === undefined) { [id, type, fn] = a; }
    const h = handlers[id] || (handlers[id] = { listeners: {} });
    h[type] = fn;
  };

  globalThis.__taskGetHandler = function (a, b) {
    let id = a, type = b;
    if (Array.isArray(a) && b === undefined) { [id, type] = a; }
    const h = handlers[id]; return h ? h[type] : undefined;
  };

  globalThis.__taskAddListener = function (a, b, c) {
    let id = a, type = b, fn = c;
    if (Array.isArray(a) && b === undefined) { [id, type, fn] = a; }
    const h = handlers[id] || (handlers[id] = { listeners: {} });
    (h.listeners[type] || (h.listeners[type] = [])).push(fn);
  };

  globalThis.__taskRemoveListener = function (a, b, c) {
    let id = a, type = b, fn = c;
    if (Array.isArray(a) && b === undefined) { [id, type, fn] = a; }
    const h = handlers[id]; if (!h || !h.listeners || !h.listeners[type]) return;
    h.listeners[type] = h.listeners[type].filter((l) => l !== fn);
  };

  globalThis.__taskDispatch = function (a, b) {
    let id = a, evt = b;
    if (Array.isArray(a) && b === undefined) { [id, evt] = a; }
    const h = handlers[id] || {};
    const t = evt && evt.type; if (!t) return;
    if (typeof h[t] === 'function') { try { h[t](evt); } catch (e) {} }
    const list = (h.listeners && h.listeners[t]) || [];
    for (const l of list) { try { l(evt); } catch (e) {} }
  };

  globalThis.__taskSetEnvData = function (a, b) {
    let key = a, val = b;
    if (Array.isArray(a) && b === undefined) { [key, val] = a; }
    if (val === undefined) envData.delete(key);
    else envData.set(key, val);
  };

  globalThis.__taskGetEnvData = function (a) {
    let key = Array.isArray(a) ? a[0] : a;
    return envData.get(key);
  };

  globalThis.__taskCreatePort = function (a) {
    let threadId = Array.isArray(a) ? a[0] : a;
    if (!wt || !wt.MessageChannel) return null;
    const mc = new wt.MessageChannel();
    threadPorts.set(threadId, mc.port1);
    if (mc.port1.unref) mc.port1.unref();
    return mc.port2;
  };

  globalThis.__taskDestroyPort = function (a) {
    let threadId = Array.isArray(a) ? a[0] : a;
    const p = threadPorts.get(threadId);
    if (p) {
      if (p.close) p.close();
      threadPorts.delete(threadId);
    }
  };

  globalThis.__taskPostToThread = function (a, b, c) {
    let threadId = a, val = b, transfer = c;
    if (Array.isArray(a) && b === undefined) { [threadId, val, transfer] = a; }
    const p = threadPorts.get(threadId);
    if (p) {
      let tList = Array.isArray(transfer) ? transfer : (transfer && Array.isArray(transfer.transfer) ? transfer.transfer : undefined);
      p.postMessage(val, tList);
    }
  };
})();
"#;

fn task_ensure_bootstrap(env: &Env) -> napi::Result<()> {
    let _: Unknown = env.run_script(TASK_BOOTSTRAP)?;
    Ok(())
}

#[napi]
impl Task {
    /// Constructs a new [`Task`] running the named JavaScript/TypeScript file
    /// in a separate thread.
    #[napi(constructor)]
    pub fn new(env: Env, script: String, options: Option<TaskOptions>) -> napi::Result<Self> {
        task_ensure_bootstrap(&env)?;
        let id = NEXT_TASK_ID.fetch_add(1, AtomicOrdering::SeqCst);
        let name = options.as_ref().and_then(|o| o.name.clone());

        let init_fn: Function<(u32, String, Option<String>), u32> =
            env.run_script("globalThis.__taskInit")?;
        let thread_id = init_fn.call((id, script.clone(), name.clone()))?;

        Ok(Task {
            id,
            script,
            thread_id,
            name,
            terminated: false,
        })
    }

    /// Script path executed by this task.
    #[napi(getter)]
    pub fn script(&self) -> String {
        self.script.clone()
    }

    /// The unique thread ID for the worker thread.
    #[napi(getter, js_name = "threadId")]
    pub fn thread_id(&self) -> u32 {
        self.thread_id
    }

    /// Optional thread name.
    #[napi(getter, js_name = "threadName")]
    pub fn thread_name(&self) -> Option<String> {
        self.name.clone()
    }

    /// Whether the task has been terminated.
    #[napi(getter)]
    pub fn terminated(&self) -> bool {
        self.terminated
    }

    /// Event handler for `message` events sent from the task thread.
    #[napi(getter, js_name = "onmessage")]
    pub fn get_onmessage<'a>(&self, env: &'a Env) -> napi::Result<Unknown<'a>> {
        let get_fn: Function<(u32, String), Unknown> =
            env.run_script("globalThis.__taskGetHandler")?;
        get_fn.call((self.id, "message".to_string()))
    }

    #[napi(setter, js_name = "onmessage")]
    pub fn set_onmessage(&self, env: Env, handler: Unknown) -> napi::Result<()> {
        let set_fn: Function<(u32, String, Unknown), ()> =
            env.run_script("globalThis.__taskSetHandler")?;
        set_fn.call((self.id, "message".to_string(), handler))?;
        Ok(())
    }

    /// Event handler for `error` events from the task thread.
    #[napi(getter, js_name = "onerror")]
    pub fn get_onerror<'a>(&self, env: &'a Env) -> napi::Result<Unknown<'a>> {
        let get_fn: Function<(u32, String), Unknown> =
            env.run_script("globalThis.__taskGetHandler")?;
        get_fn.call((self.id, "error".to_string()))
    }

    #[napi(setter, js_name = "onerror")]
    pub fn set_onerror(&self, env: Env, handler: Unknown) -> napi::Result<()> {
        let set_fn: Function<(u32, String, Unknown), ()> =
            env.run_script("globalThis.__taskSetHandler")?;
        set_fn.call((self.id, "error".to_string(), handler))?;
        Ok(())
    }

    /// Event handler for `messageerror` events from the task thread.
    #[napi(getter, js_name = "onmessageerror")]
    pub fn get_onmessageerror<'a>(&self, env: &'a Env) -> napi::Result<Unknown<'a>> {
        let get_fn: Function<(u32, String), Unknown> =
            env.run_script("globalThis.__taskGetHandler")?;
        get_fn.call((self.id, "messageerror".to_string()))
    }

    #[napi(setter, js_name = "onmessageerror")]
    pub fn set_onmessageerror(&self, env: Env, handler: Unknown) -> napi::Result<()> {
        let set_fn: Function<(u32, String, Unknown), ()> =
            env.run_script("globalThis.__taskSetHandler")?;
        set_fn.call((self.id, "messageerror".to_string(), handler))?;
        Ok(())
    }

    /// Posts a message to the task thread.
    #[napi(js_name = "postMessage")]
    pub fn post_message(
        &self,
        env: Env,
        message: Unknown,
        transfer: Option<Unknown>,
    ) -> napi::Result<()> {
        let post_fn: Function<(u32, Unknown, Option<Unknown>), ()> =
            env.run_script("globalThis.__taskPost")?;
        post_fn.call((self.id, message, transfer))?;
        Ok(())
    }

    /// Terminates the worker thread.
    #[napi]
    pub fn terminate(&mut self, env: Env) -> napi::Result<()> {
        self.terminated = true;
        let term_fn: Function<u32, ()> = env.run_script("globalThis.__taskTerminate")?;
        term_fn.call(self.id)?;
        Ok(())
    }

    /// Keep the process event loop active for this task.
    #[napi(js_name = "ref")]
    pub fn ref_task(&self, env: Env) -> napi::Result<()> {
        let ref_fn: Function<u32, ()> = env.run_script("globalThis.__taskRef")?;
        ref_fn.call(self.id)?;
        Ok(())
    }

    /// Allow the process event loop to exit if this task is the only active handle left.
    #[napi(js_name = "unref")]
    pub fn unref_task(&self, env: Env) -> napi::Result<()> {
        let unref_fn: Function<u32, ()> = env.run_script("globalThis.__taskUnref")?;
        unref_fn.call(self.id)?;
        Ok(())
    }

    /// Appends an event listener for events of the specified `type`.
    #[napi(js_name = "addEventListener")]
    pub fn add_event_listener(
        &self,
        env: Env,
        type_: String,
        listener: Unknown,
        _options: Option<Unknown>,
    ) -> napi::Result<()> {
        let add_fn: Function<(u32, String, Unknown), ()> =
            env.run_script("globalThis.__taskAddListener")?;
        add_fn.call((self.id, type_, listener))?;
        Ok(())
    }

    /// Removes an event listener for events of the specified `type`.
    #[napi(js_name = "removeEventListener")]
    pub fn remove_event_listener(
        &self,
        env: Env,
        type_: String,
        listener: Unknown,
        _options: Option<Unknown>,
    ) -> napi::Result<()> {
        let rm_fn: Function<(u32, String, Unknown), ()> =
            env.run_script("globalThis.__taskRemoveListener")?;
        rm_fn.call((self.id, type_, listener))?;
        Ok(())
    }

    /// Dispatches a synthetic event.
    #[napi(js_name = "dispatchEvent")]
    pub fn dispatch_event(&self, env: Env, event: Unknown) -> napi::Result<bool> {
        let disp_fn: Function<(u32, Unknown), ()> =
            env.run_script("globalThis.__taskDispatch")?;
        disp_fn.call((self.id, event))?;
        Ok(true)
    }
}

/// Returns whether the current thread is the main thread.
#[napi(js_name = "isMainThread")]
pub fn is_main_thread(env: Env) -> napi::Result<bool> {
    task_ensure_bootstrap(&env)?;
    let res: Unknown = env.run_script(
        "(function() { try { const wt = process.getBuiltinModule ? process.getBuiltinModule('node:worker_threads') : require('node:worker_threads'); return wt.isMainThread; } catch (e) { return true; } })()",
    )?;
    match res.get_type()? {
        napi::ValueType::Boolean => {
            let b: bool = unsafe { res.cast()? };
            Ok(b)
        }
        _ => Ok(true),
    }
}

/// Returns the current thread ID.
#[napi(js_name = "getThreadId")]
pub fn get_thread_id(env: Env) -> napi::Result<u32> {
    task_ensure_bootstrap(&env)?;
    let res: Unknown = env.run_script(
        "(function() { try { const wt = process.getBuiltinModule ? process.getBuiltinModule('node:worker_threads') : require('node:worker_threads'); return wt.threadId; } catch (e) { return 0; } })()",
    )?;
    match res.get_type()? {
        napi::ValueType::Number => {
            let num: u32 = unsafe { res.cast()? };
            Ok(num)
        }
        _ => Ok(0),
    }
}

/// Sets environment data shared across task threads.
#[napi(js_name = "setEnvironmentData")]
pub fn set_environment_data(env: Env, key: Unknown, value: Unknown) -> napi::Result<()> {
    task_ensure_bootstrap(&env)?;
    let set_fn: Function<(Unknown, Unknown), ()> =
        env.run_script("globalThis.__taskSetEnvData")?;
    set_fn.call((key, value))?;
    Ok(())
}

/// Gets environment data shared across task threads.
#[napi(js_name = "getEnvironmentData")]
pub fn get_environment_data<'a>(env: &'a Env, key: Unknown) -> napi::Result<Unknown<'a>> {
    task_ensure_bootstrap(env)?;
    let get_fn: Function<Unknown, Unknown> = env.run_script("globalThis.__taskGetEnvData")?;
    get_fn.call(key)
}

/// Creates a MessagePort linking a target thread to the main thread.
#[napi(js_name = "createMainThreadPort")]
pub fn create_main_thread_port<'a>(env: &'a Env, thread_id: u32) -> napi::Result<Unknown<'a>> {
    task_ensure_bootstrap(env)?;
    let create_fn: Function<u32, Unknown> = env.run_script("globalThis.__taskCreatePort")?;
    create_fn.call(thread_id)
}

/// Destroys a MessagePort linked to a target thread.
#[napi(js_name = "destroyMainThreadPort")]
pub fn destroy_main_thread_port(env: Env, thread_id: u32) -> napi::Result<()> {
    task_ensure_bootstrap(&env)?;
    let destroy_fn: Function<u32, ()> = env.run_script("globalThis.__taskDestroyPort")?;
    destroy_fn.call(thread_id)?;
    Ok(())
}

/// Posts a message directly to a target thread port.
#[napi(js_name = "postMessageToThread")]
pub fn post_message_to_thread(
    env: Env,
    thread_id: u32,
    value: Unknown,
    transfer: Option<Unknown>,
) -> napi::Result<()> {
    task_ensure_bootstrap(&env)?;
    let post_fn: Function<(u32, Unknown, Option<Unknown>), ()> =
        env.run_script("globalThis.__taskPostToThread")?;
    post_fn.call((thread_id, value, transfer))?;
    Ok(())
}
