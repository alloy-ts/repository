use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use napi::bindgen_prelude::{Array, Buffer, FnArgs, FromNapiValue, Function, JsObjectValue, Object};
use napi::sys;
use napi::{Env, JsValue, Unknown, ValueType};
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
#[napi]
pub struct Task {
    /// Process-unique id used to key the worker and its handlers.
    id: u32,
    /// The `Worker.threadId` assigned by Node when the worker starts.
    thread_id: u32,
    /// Whether [`Task::terminate`] has been called.
    terminated: Cell<bool>,
}

/// Monotonic, process-unique id for each [`Task`] instance.
static NEXT_TASK_ID: AtomicU32 = AtomicU32::new(1);

fn undefined_unknown(env: &Env) -> Unknown<'static> {
    let mut raw = std::ptr::null_mut();
    unsafe {
        sys::napi_get_undefined(env.raw(), &mut raw);
        Unknown::from_raw_unchecked(env.raw(), raw)
    }
}

fn get_worker_init_fn<'a>(env: &'a Env) -> napi::Result<Function<'a, FnArgs<(String, Object<'a>, u32)>, Unknown<'a>>> {
    let mut global = env.get_global()?;
    if let Ok(u) = global.get_named_property::<Unknown>("__taskInitFn") {
        if u.get_type()? == ValueType::Function {
            return unsafe { Function::from_napi_value(env.raw(), u.raw()) };
        }
    }
    let init_fn: Function<FnArgs<(String, Object, u32)>, Unknown> = env.run_script(
        "((script, opts, id) => {
            const getWT = () => {
                if (typeof globalThis.__require === 'function') return globalThis.__require('node:worker_threads');
                if (typeof process.getBuiltinModule === 'function') return process.getBuiltinModule('node:worker_threads');
                if (process.mainModule && typeof process.mainModule.require === 'function') return process.mainModule.require('node:worker_threads');
                try { return require('node:worker_threads'); } catch {}
                return require('node:module').createRequire(process.cwd() + '/')('node:worker_threads');
            };
            const wt = getWT();
            const w = new wt.Worker(script, opts);
            const fire = (type, evt) => {
                const handlers = (globalThis.__taskHandlers || {})[id] || {};
                if (typeof handlers[type] === 'function') { try { handlers[type](evt); } catch (e) {} }
                const list = (handlers.listeners && handlers.listeners[type]) || [];
                for (const l of list) { try { l(evt); } catch (e) {} }
            };
            w.on('message', (d) => fire('message', { data: d }));
            w.on('error', (e) => fire('error', { error: e, message: e && e.message }));
            w.on('messageerror', (e) => fire('messageerror', { error: e }));
            w.on('exit', (code, sig) => fire('exit', { code, signal: sig }));
            return w;
        })"
    )?;
    let fn_unknown = unsafe { Unknown::from_raw_unchecked(env.raw(), init_fn.raw()) };
    global.set_named_property("__taskInitFn", fn_unknown)?;
    Ok(init_fn)
}

fn create_worker_instance<'a>(env: &'a Env, script: String, options: Object<'a>, id: u32) -> napi::Result<Object<'a>> {
    let init_fn = get_worker_init_fn(env)?;
    let val = init_fn.call((script, options, id).into())?;
    unsafe { Object::from_napi_value(env.raw(), val.raw()) }
}

fn get_task_registries(env: &Env) -> napi::Result<(Object<'_>, Object<'_>)> {
    let mut global = env.get_global()?;
    let workers: Object = match global.get_named_property("__taskWorkers") {
        Ok(obj) => obj,
        Err(_) => {
            let obj = Object::new(env)?;
            global.set_named_property("__taskWorkers", obj.clone())?;
            obj
        }
    };
    let handlers: Object = match global.get_named_property("__taskHandlers") {
        Ok(obj) => obj,
        Err(_) => {
            let obj = Object::new(env)?;
            global.set_named_property("__taskHandlers", obj.clone())?;
            obj
        }
    };
    Ok((workers, handlers))
}

fn get_task_worker(env: &Env, id: u32) -> napi::Result<Option<Object<'_>>> {
    let (workers, _) = get_task_registries(env)?;
    let id_str = id.to_string();
    match workers.get_named_property::<Unknown>(&id_str) {
        Ok(u) => match u.get_type()? {
            ValueType::Object => Ok(Some(unsafe { Object::from_napi_value(env.raw(), u.raw())? })),
            _ => Ok(None),
        },
        Err(_) => Ok(None),
    }
}

fn fire_task_event(env: &Env, id: u32, event_type: &str, event_obj: Object) -> napi::Result<()> {
    let (_, handlers) = get_task_registries(env)?;
    let id_str = id.to_string();
    if let Ok(h) = handlers.get_named_property::<Object>(&id_str) {
        if let Ok(fn_val) = h.get_named_property::<Function<Object, Unknown>>(event_type) {
            let _ = fn_val.call(event_obj.clone());
        }
        if let Ok(listeners) = h.get_named_property::<Object>("listeners") {
            if let Ok(list) = listeners.get_named_property::<Array>(event_type) {
                let len = list.len();
                for i in 0..len {
                    if let Ok(listener) = list.get_element::<Function<Object, Unknown>>(i) {
                        let _ = listener.call(event_obj.clone());
                    }
                }
            }
        }
    }
    Ok(())
}

fn task_set_handler(env: &Env, id: u32, type_: &str, handler: Option<Function<'static, (), ()>>) -> napi::Result<()> {
    let (_, mut handlers) = get_task_registries(env)?;
    let id_str = id.to_string();
    let mut h: Object = match handlers.get_named_property::<Unknown>(&id_str) {
        Ok(u) if u.get_type()? == ValueType::Object => unsafe { Object::from_napi_value(env.raw(), u.raw())? },
        _ => {
            let mut obj = Object::new(env)?;
            let listeners = Object::new(env)?;
            obj.set("listeners", listeners)?;
            handlers.set(&id_str, obj.clone())?;
            obj
        }
    };
    match handler {
        Some(f) => h.set(type_, f)?,
        None => {
            let undefined = undefined_unknown(env);
            h.set(type_, undefined)?;
        }
    }
    Ok(())
}

fn task_get_handler(env: &Env, id: u32, type_: &str) -> napi::Result<Option<Function<'static, (), ()>>> {
    let (_, handlers) = get_task_registries(env)?;
    let id_str = id.to_string();
    if let Ok(h) = handlers.get_named_property::<Object>(&id_str) {
        if let Ok(u) = h.get_named_property::<Unknown>(type_) {
            if u.get_type()? == ValueType::Function {
                return Ok(Some(unsafe { Function::from_napi_value(env.raw(), u.raw())? }));
            }
        }
    }
    Ok(None)
}

fn task_call_diag(
    env: &Env,
    id: u32,
    method: &str,
    arg: Option<Unknown<'static>>,
) -> napi::Result<Unknown<'static>> {
    if let Some(worker) = get_task_worker(env, id)? {
        let res = match arg {
            Some(a) => {
                let fn_val: Function<Unknown<'static>, Unknown<'static>> = worker.get_named_property(method)?;
                fn_val.apply(Some(&worker), a)?
            }
            None => {
                let fn_val: Function<(), Unknown<'static>> = worker.get_named_property(method)?;
                fn_val.apply(Some(&worker), ())?
            }
        };
        Ok(res)
    } else {
        Ok(undefined_unknown(env))
    }
}

#[napi]
impl Task {
    /// Create a task that runs `script` (a file path understood by
    /// `node:worker_threads`, e.g. `"build.js"`) on a dedicated worker thread.
    #[napi(constructor)]
    pub fn new(env: Env, script: String, options: Option<TaskOptions>) -> napi::Result<Self> {
        let mut opts_obj = Object::new(&env)?;
        if let Some(opts) = options {
            if let Some(name) = opts.name {
                opts_obj.set("name", name)?;
            }
            if let Some(rl) = opts.resource_limits {
                let mut rl_obj = Object::new(&env)?;
                if let Some(v) = rl.max_young_generation_size_mb {
                    rl_obj.set("maxYoungGenerationSizeMb", v)?;
                }
                if let Some(v) = rl.max_old_generation_size_mb {
                    rl_obj.set("maxOldGenerationSizeMb", v)?;
                }
                if let Some(v) = rl.code_range_size_mb {
                    rl_obj.set("codeRangeSizeMb", v)?;
                }
                if let Some(v) = rl.stack_size_mb {
                    rl_obj.set("stackSizeMb", v)?;
                }
                opts_obj.set("resourceLimits", rl_obj)?;
            }
            if let Some(argv) = opts.argv {
                opts_obj.set("argv", argv)?;
            }
            if let Some(exec_argv) = opts.exec_argv {
                opts_obj.set("execArgv", exec_argv)?;
            }
            if let Some(wd) = opts.worker_data {
                let val = env.to_js_value(&wd)?;
                opts_obj.set("workerData", val)?;
            }
            if let Some(env_val) = opts.env {
                let val = env.to_js_value(&env_val)?;
                opts_obj.set("env", val)?;
            }
            if let Some(track) = opts.track_unmanaged_fds {
                opts_obj.set("trackUnmanagedFds", track)?;
            }
        }

        let id = NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed);
        let worker = create_worker_instance(&env, script, opts_obj, id)?;
        let thread_id: u32 = worker.get_named_property("threadId")?;

        let (mut workers, _handlers) = get_task_registries(&env)?;
        let id_str = id.to_string();
        workers.set(&id_str, worker)?;

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
        if let Some(worker) = get_task_worker(&env, self.id)? {
            match transfer {
                Some(t) => {
                    let mut arr = env.create_array(t.len() as u32)?;
                    for (i, item) in t.into_iter().enumerate() {
                        arr.set_element(i as u32, item)?;
                    }
                    let post_msg: Function<FnArgs<(Unknown<'static>, Array)>, Unknown> = worker.get_named_property("postMessage")?;
                    post_msg.apply(Some(&worker), (message, arr).into())?;
                }
                None => {
                    let post_msg: Function<Unknown<'static>, Unknown> = worker.get_named_property("postMessage")?;
                    post_msg.apply(Some(&worker), message)?;
                }
            }
        }
        Ok(())
    }

    /// Terminate the worker thread. Resolves to the worker's exit code once it
    /// has exited — mirrors C++ `Task::StopThread` / `JoinThread`.
    #[napi(ts_return_type = "Promise<number>")]
    pub fn terminate(&self, env: Env) -> napi::Result<Unknown<'static>> {
        self.terminated.set(true);
        if let Some(worker) = get_task_worker(&env, self.id)? {
            let term: Function<(), Unknown<'static>> = worker.get_named_property("terminate")?;
            term.apply(Some(&worker), ())
        } else {
            Ok(undefined_unknown(&env))
        }
    }

    /// Increment the worker's reference count so it keeps the process alive
    /// (default). Mirrors C++ `Task::Ref`.
    #[napi]
    pub fn reference(&self, env: Env) -> napi::Result<()> {
        if let Some(worker) = get_task_worker(&env, self.id)? {
            if let Ok(ref_fn) = worker.get_named_property::<Function<(), ()>>("ref") {
                let _ = ref_fn.apply(Some(&worker), ());
            }
        }
        Ok(())
    }

    /// Decrement the worker's reference count so the process may exit even while
    /// the worker is running. Mirrors C++ `Task::Unref`.
    #[napi]
    pub fn unreference(&self, env: Env) -> napi::Result<()> {
        if let Some(worker) = get_task_worker(&env, self.id)? {
            if let Ok(unref_fn) = worker.get_named_property::<Function<(), ()>>("unref") {
                let _ = unref_fn.apply(Some(&worker), ());
            }
        }
        Ok(())
    }

    #[napi(getter)]
    pub fn get_onmessage(&self, env: Env) -> napi::Result<Option<Function<'static, (), ()>>> {
        task_get_handler(&env, self.id, "message")
    }

    #[napi(setter)]
    pub fn set_onmessage(&self, env: Env, handler: Option<Function<'static, (), ()>>) -> napi::Result<()> {
        task_set_handler(&env, self.id, "message", handler)
    }

    #[napi(getter)]
    pub fn get_onerror(&self, env: Env) -> napi::Result<Option<Function<'static, (), ()>>> {
        task_get_handler(&env, self.id, "error")
    }

    #[napi(setter)]
    pub fn set_onerror(&self, env: Env, handler: Option<Function<'static, (), ()>>) -> napi::Result<()> {
        task_set_handler(&env, self.id, "error", handler)
    }

    #[napi(getter)]
    pub fn get_onmessageerror(&self, env: Env) -> napi::Result<Option<Function<'static, (), ()>>> {
        task_get_handler(&env, self.id, "messageerror")
    }

    #[napi(setter)]
    pub fn set_onmessageerror(&self, env: Env, handler: Option<Function<'static, (), ()>>) -> napi::Result<()> {
        task_set_handler(&env, self.id, "messageerror", handler)
    }

    /// Register an event listener (mirrors `EventTarget.addEventListener`).
    #[napi]
    pub fn add_event_listener(
        &self,
        env: Env,
        type_: String,
        listener: Function<'static, (), ()>,
    ) -> napi::Result<()> {
        let (_, mut handlers) = get_task_registries(&env)?;
        let id_str = self.id.to_string();
        let mut h: Object = match handlers.get_named_property::<Unknown>(&id_str) {
            Ok(u) if u.get_type()? == ValueType::Object => unsafe { Object::from_napi_value(env.raw(), u.raw())? },
            _ => {
                let mut obj = Object::new(&env)?;
                let listeners = Object::new(&env)?;
                obj.set("listeners", listeners)?;
                handlers.set(&id_str, obj.clone())?;
                obj
            }
        };
        let mut listeners: Object = match h.get_named_property::<Unknown>("listeners") {
            Ok(u) if u.get_type()? == ValueType::Object => unsafe { Object::from_napi_value(env.raw(), u.raw())? },
            _ => {
                let obj = Object::new(&env)?;
                h.set("listeners", obj.clone())?;
                obj
            }
        };
        let mut list: Array = match listeners.get_named_property::<Unknown>(&type_) {
            Ok(u) if u.get_type()? == ValueType::Object => unsafe { Array::from_napi_value(env.raw(), u.raw())? },
            _ => {
                let arr = env.create_array(0)?;
                listeners.set(&type_, arr.clone())?;
                arr
            }
        };
        let len = list.len();
        list.set_element(len, listener)?;
        Ok(())
    }

    /// Remove a previously registered event listener.
    #[napi]
    pub fn remove_event_listener(
        &self,
        env: Env,
        type_: String,
        listener: Function<'static, (), ()>,
    ) -> napi::Result<()> {
        let (_, handlers) = get_task_registries(&env)?;
        let id_str = self.id.to_string();
        if let Ok(h) = handlers.get_named_property::<Object>(&id_str) {
            if let Ok(mut listeners) = h.get_named_property::<Object>("listeners") {
                if let Ok(list) = listeners.get_named_property::<Array>(&type_) {
                    let len = list.len();
                    let mut new_list = env.create_array(0)?;
                    let mut new_idx = 0u32;
                    let target_raw = listener.raw();
                    for i in 0..len {
                        if let Ok(item) = list.get_element::<Function<'static, (), ()>>(i) {
                            let mut is_equal = false;
                            unsafe {
                                sys::napi_strict_equals(env.raw(), item.raw(), target_raw, &mut is_equal);
                            }
                            if !is_equal {
                                new_list.set_element(new_idx, item)?;
                                new_idx += 1;
                            }
                        }
                    }
                    listeners.set(&type_, new_list)?;
                }
            }
        }
        Ok(())
    }

    /// Dispatch an event to this task's listeners (mirrors `EventTarget.dispatchEvent`).
    #[napi]
    pub fn dispatch_event(&self, env: Env, evt: Unknown<'static>) -> napi::Result<()> {
        if let Ok(evt_obj) = unsafe { Object::from_napi_value(env.raw(), evt.raw()) } {
            if let Ok(type_val) = evt_obj.get_named_property::<Unknown>("type") {
                if type_val.get_type()? == ValueType::String {
                    let type_str: String = env.from_js_value(type_val)?;
                    fire_task_event(&env, self.id, &type_str, evt_obj)?;
                }
            }
        }
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
        task_call_diag(&env, self.id, "getHeapSnapshot", options)
    }

    /// Return the worker's V8 heap statistics. Mirrors C++ `Task::GetHeapStatistics`.
    #[napi(ts_return_type = "Promise<object>")]
    pub fn get_heap_statistics(&self, env: Env) -> napi::Result<Unknown<'static>> {
        task_call_diag(&env, self.id, "getHeapStatistics", None)
    }

    /// CPU time used by the worker so far (`{ user, system }` in microseconds).
    /// Mirrors C++ `Task::CpuUsage`.
    #[napi(ts_return_type = "{ user: number, system: number }")]
    pub fn cpu_usage(
        &self,
        env: Env,
        prev: Option<Unknown<'static>>,
    ) -> napi::Result<Unknown<'static>> {
        task_call_diag(&env, self.id, "cpuUsage", prev)
    }

    /// Event-loop utilization of the worker. Mirrors C++ `Task::LoopIdleTime`.
    #[napi(ts_return_type = "object")]
    pub fn event_loop_utilization(
        &self,
        env: Env,
        prev: Option<Unknown<'static>>,
    ) -> napi::Result<Unknown<'static>> {
        if let Some(worker) = get_task_worker(&env, self.id)? {
            if let Ok(perf) = worker.get_named_property::<Object>("performance") {
                let res = match prev {
                    Some(p) => {
                        let fn_val: Function<Unknown<'static>, Unknown<'static>> = perf.get_named_property("eventLoopUtilization")?;
                        fn_val.apply(Some(&perf), p)?
                    }
                    None => {
                        let fn_val: Function<(), Unknown<'static>> = perf.get_named_property("eventLoopUtilization")?;
                        fn_val.apply(Some(&perf), ())?
                    }
                };
                return Ok(res);
            }
        }
        Ok(undefined_unknown(&env))
    }

    /// Start a CPU profile of the worker. Mirrors C++ `Task::StartCpuProfile`.
    #[napi(ts_return_type = "unknown")]
    pub fn start_cpu_profile(
        &self,
        env: Env,
        options: Option<Unknown<'static>>,
    ) -> napi::Result<Unknown<'static>> {
        task_call_diag(&env, self.id, "startCpuProfile", options)
    }

    /// Stop a CPU profile; resolves to the profile as a JSON string.
    #[napi(ts_return_type = "Promise<string>")]
    pub fn stop_cpu_profile(
        &self,
        env: Env,
        options: Option<Unknown<'static>>,
    ) -> napi::Result<Unknown<'static>> {
        task_call_diag(&env, self.id, "stopCpuProfile", options)
    }

    /// Start a heap profile of the worker. Mirrors C++ `Task::StartHeapProfile`.
    #[napi(ts_return_type = "unknown")]
    pub fn start_heap_profile(
        &self,
        env: Env,
        options: Option<Unknown<'static>>,
    ) -> napi::Result<Unknown<'static>> {
        task_call_diag(&env, self.id, "startHeapProfile", options)
    }

    /// Stop a heap profile; resolves to the profile as a JSON string.
    #[napi(ts_return_type = "Promise<string>")]
    pub fn stop_heap_profile(
        &self,
        env: Env,
        options: Option<Unknown<'static>>,
    ) -> napi::Result<Unknown<'static>> {
        task_call_diag(&env, self.id, "stopHeapProfile", options)
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
