use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use napi::bindgen_prelude::{Function, Unknown};
use napi::{Env, Result};
use napi_derive::napi;

use crate::task::process::{CommandOptions, Process};

/// Scheduling policies for cluster connection distribution.
pub const SCHED_NONE: u32 = 1;
pub const SCHED_RR: u32 = 2;

use serde::Serialize;

#[derive(Clone, Default, Serialize)]
#[napi(object)]
pub struct ClusterSettings {
    pub exec_argv: Option<Vec<String>>,
    pub exec: Option<String>,
    pub args: Option<Vec<String>>,
    pub cwd: Option<String>,
    pub serialization: Option<String>,
    pub silent: Option<bool>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub inspect_port: Option<u32>,
    pub windows_hide: Option<bool>,
}

/// A cluster worker task.
#[napi(js_name = "ClusterTask")]
pub struct ClusterTask {
    id: u32,
    exited_after_disconnect: Arc<AtomicBool>,
    child_id: u32,
    process: Arc<Mutex<Option<Process>>>,
    dead: Arc<AtomicBool>,
    is_worker_self: bool,
}

#[napi]
impl ClusterTask {
    #[napi(getter)]
    pub fn id(&self) -> u32 {
        self.id
    }

    #[napi(getter)]
    pub fn exited_after_disconnect(&self) -> bool {
        self.exited_after_disconnect.load(Ordering::SeqCst)
    }

    #[napi(getter)]
    pub fn process(&self) -> Option<Process> {
        let guard = self.process.lock().unwrap();
        if let Some(p) = guard.as_ref() {
            Some(Process::new_handle_clone(p))
        } else {
            None
        }
    }

    #[napi]
    pub fn is_connected(&self) -> bool {
        if self.is_worker_self {
            // In worker, process.connected indicates IPC state
            if let Ok(val) = std::env::var("NODE_UNIQUE_ID") {
                if !val.is_empty() && !self.exited_after_disconnect.load(Ordering::SeqCst) {
                    return !self.dead.load(Ordering::SeqCst);
                }
            }
            return false;
        }
        let guard = self.process.lock().unwrap();
        if let Some(p) = guard.as_ref() {
            p.is_connected()
        } else {
            false
        }
    }

    #[napi]
    pub fn is_dead(&self) -> bool {
        if self.dead.load(Ordering::SeqCst) {
            return true;
        }
        let guard = self.process.lock().unwrap();
        if let Some(p) = guard.as_ref() {
            p.is_dead()
        } else {
            false
        }
    }

    #[napi]
    pub fn send(&self, env: Env, message: Unknown) -> Result<bool> {
        if self.is_worker_self {
            let send_script = r#"
                (function(msg) {
                    if (typeof process.send === 'function') {
                        return process.send(msg);
                    }
                    return false;
                })
            "#;
            let sender: Function<Unknown, bool> = env.run_script(send_script)?;
            return sender.call(message);
        }
        let guard = self.process.lock().unwrap();
        if let Some(p) = guard.as_ref() {
            p.send(env, message)
        } else {
            Ok(false)
        }
    }

    #[napi]
    pub fn kill(&self, env: Env, signal: Option<String>) -> Result<bool> {
        self.dead.store(true, Ordering::SeqCst);
        let sig = signal.unwrap_or_else(|| "SIGTERM".to_string());
        if self.is_worker_self {
            let kill_script = r#"
                (function(sig) {
                    if (typeof process.kill === 'function') {
                        process.kill(process.pid, sig);
                        return true;
                    }
                    process.exit(0);
                    return true;
                })
            "#;
            let killer: Function<String, bool> = env.run_script(kill_script)?;
            return killer.call(sig);
        }
        let guard = self.process.lock().unwrap();
        if let Some(p) = guard.as_ref() {
            p.kill(env, Some(sig))
        } else {
            Ok(false)
        }
    }

    #[napi]
    pub fn destroy(&self, env: Env, signal: Option<String>) -> Result<bool> {
        self.kill(env, signal)
    }

    #[napi]
    pub fn disconnect(&self, env: Env) -> Result<()> {
        self.exited_after_disconnect.store(true, Ordering::SeqCst);
        if self.is_worker_self {
            let disc_script = r#"
                (function() {
                    if (typeof process.disconnect === 'function') {
                        process.disconnect();
                    }
                })()
            "#;
            let _: () = env.run_script(disc_script)?;
            return Ok(());
        }
        let guard = self.process.lock().unwrap();
        if let Some(p) = guard.as_ref() {
            p.disconnect(env)?;
        }
        Ok(())
    }

    #[napi]
    pub fn on(
        &self,
        env: Env,
        event: String,
        listener: Unknown,
    ) -> Result<()> {
        let script = r#"
            (function(taskId, ev) {
                if (!globalThis.__clusterTaskEmitters) globalThis.__clusterTaskEmitters = new Map();
                let emitter = globalThis.__clusterTaskEmitters.get(taskId);
                if (!emitter) {
                    const events = (process.getBuiltinModule ? process.getBuiltinModule('node:events') : null)
                        || (typeof require === 'function' ? require('node:events') : null)
                        || (process.mainModule ? process.mainModule.require('node:events') : null);
                    emitter = new events.EventEmitter();
                    globalThis.__clusterTaskEmitters.set(taskId, emitter);
                }
                return function(cb) {
                    emitter.on(ev, function(...args) {
                        if (typeof cb === 'function') cb(...args);
                    });
                };
            })
        "#;
        let builder: Function<(u32, String), Function<'static, Unknown, ()>> =
            env.run_script(script)?;
        let registrator = builder.call((self.id, event))?;
        registrator.call(listener)
    }

    #[napi]
    pub fn add_event_listener(
        &self,
        env: Env,
        event: String,
        listener: Unknown,
    ) -> Result<()> {
        self.on(env, event, listener)
    }

    pub(crate) fn clone_task(&self) -> Self {
        Self {
            id: self.id,
            exited_after_disconnect: Arc::clone(&self.exited_after_disconnect),
            child_id: self.child_id,
            process: Arc::clone(&self.process),
            dead: Arc::clone(&self.dead),
            is_worker_self: self.is_worker_self,
        }
    }
}

/// Global cluster coordinator state.
struct ClusterState {
    settings: Mutex<ClusterSettings>,
    tasks: Mutex<HashMap<u32, ClusterTask>>,
    scheduling_policy: AtomicU32,
    next_task_id: AtomicU32,
}

static CLUSTER_STATE: OnceLock<ClusterState> = OnceLock::new();

fn cluster_state() -> &'static ClusterState {
    CLUSTER_STATE.get_or_init(|| {
        let default_policy = if cfg!(windows) {
            SCHED_NONE
        } else {
            SCHED_RR
        };
        let policy = if let Ok(val) = std::env::var("NODE_CLUSTER_SCHED_POLICY") {
            match val.to_lowercase().as_str() {
                "none" => SCHED_NONE,
                "rr" => SCHED_RR,
                _ => default_policy,
            }
        } else {
            default_policy
        };

        ClusterState {
            settings: Mutex::new(ClusterSettings::default()),
            tasks: Mutex::new(HashMap::new()),
            scheduling_policy: AtomicU32::new(policy),
            next_task_id: AtomicU32::new(1),
        }
    })
}

/// The `Cluster` class mirroring `@alloy-ts/task/cluster`.
#[napi]
pub struct Cluster {}

#[napi]
impl Cluster {
    #[napi(constructor)]
    pub fn new() -> Self {
        Cluster {}
    }

    #[napi(getter)]
    pub fn is_primary(&self) -> bool {
        std::env::var("NODE_UNIQUE_ID").is_err()
    }

    #[napi(getter)]
    pub fn is_master(&self) -> bool {
        self.is_primary()
    }

    #[napi(getter)]
    pub fn is_worker(&self) -> bool {
        !self.is_primary()
    }

    #[napi(getter)]
    pub fn istask(&self) -> bool {
        self.is_worker()
    }

    #[napi(getter)]
    pub fn scheduling_policy(&self) -> u32 {
        cluster_state().scheduling_policy.load(Ordering::SeqCst)
    }

    #[napi(setter)]
    pub fn set_scheduling_policy(&self, policy: u32) {
        cluster_state().scheduling_policy.store(policy, Ordering::SeqCst);
    }

    #[napi(getter)]
    pub fn get_sched_none(&self) -> u32 {
        SCHED_NONE
    }

    #[napi(getter)]
    pub fn get_sched_rr(&self) -> u32 {
        SCHED_RR
    }

    #[napi(getter)]
    pub fn settings(&self) -> ClusterSettings {
        cluster_state().settings.lock().unwrap().clone()
    }

    #[napi]
    pub fn setup_primary(&self, env: Env, settings: Option<ClusterSettings>) -> Result<()> {
        if let Some(s) = settings {
            let mut guard = cluster_state().settings.lock().unwrap();
            *guard = s.clone();
            drop(guard);

            // Emit 'setup' event on cluster
            let setup_script = r#"
                (function(settings) {
                    if (globalThis.__clusterEmitter && typeof globalThis.__clusterEmitter.emit === 'function') {
                        globalThis.__clusterEmitter.emit('setup', settings);
                    }
                })
            "#;
            let runner: Function<Option<ClusterSettings>, ()> = env.run_script(setup_script)?;
            let _ = runner.call(Some(s));
        }
        Ok(())
    }

    #[napi]
    pub fn setup_master(&self, env: Env, settings: Option<ClusterSettings>) -> Result<()> {
        self.setup_primary(env, settings)
    }

    /// Fork a new cluster worker task process.
    #[napi]
    pub fn fork(&self, env: Env, custom_env: Option<HashMap<String, String>>) -> Result<ClusterTask> {
        let state = cluster_state();
        let task_id = state.next_task_id.fetch_add(1, Ordering::SeqCst);

        let settings = state.settings.lock().unwrap().clone();

        let forker_builder = r#"
            (function(payload) {
                const taskId = payload.taskId;
                const settings = payload.settings;
                const customEnv = payload.customEnv;

                const cp = (process.getBuiltinModule ? process.getBuiltinModule('node:child_process') : null)
                    || (typeof require === 'function' ? require('node:child_process') : null)
                    || (process.mainModule ? process.mainModule.require('node:child_process') : null);
                const events = (process.getBuiltinModule ? process.getBuiltinModule('node:events') : null)
                    || (typeof require === 'function' ? require('node:events') : null)
                    || (process.mainModule ? process.mainModule.require('node:events') : null);
                if (!globalThis.__subprocesses) globalThis.__subprocesses = new Map();
                if (!globalThis.__clusterEmitter) {
                    globalThis.__clusterEmitter = new events.EventEmitter();
                }
                if (!globalThis.__clusterTasks) globalThis.__clusterTasks = new Map();
                if (!globalThis.__clusterTaskEmitters) globalThis.__clusterTaskEmitters = new Map();
                const taskEmitter = new events.EventEmitter();
                globalThis.__clusterTaskEmitters.set(taskId, taskEmitter);

                const exec = (settings && settings.exec) || process.argv[1] || '';
                const args = (settings && settings.args) || process.argv.slice(2) || [];
                const execArgv = (settings && settings.execArgv) || process.execArgv || [];

                const env = Object.assign({}, process.env, customEnv || {}, {
                    NODE_UNIQUE_ID: String(taskId),
                });

                const forkOptions = {
                    env,
                    execArgv,
                    silent: settings && Boolean(settings.silent),
                };
                if (settings && settings.cwd) forkOptions.cwd = settings.cwd;
                if (settings && settings.serialization) forkOptions.serialization = settings.serialization;

                let child;
                if (exec === 'node' || exec === process.execPath) {
                    child = cp.fork(args[0] || '', args.slice(1), forkOptions);
                } else {
                    try {
                        child = cp.fork(exec, args, forkOptions);
                    } catch (e) {
                        child = cp.spawn(exec || process.execPath, args, {
                            env,
                            stdio: ['pipe', 'pipe', 'pipe', 'ipc']
                        });
                    }
                }

                globalThis.__subprocesses.set(taskId, child);

                // Forward child events to taskEmitter and clusterEmitter
                child.on('message', (msg, handle) => {
                    const taskObj = globalThis.__clusterTasks.get(taskId);
                    taskEmitter.emit('message', msg, handle);
                    globalThis.__clusterEmitter.emit('message', taskObj, msg, handle);
                });
                child.on('disconnect', () => {
                    const taskObj = globalThis.__clusterTasks.get(taskId);
                    taskEmitter.emit('disconnect');
                    globalThis.__clusterEmitter.emit('disconnect', taskObj);
                });
                child.on('error', (err) => {
                    taskEmitter.emit('error', err);
                });
                child.on('exit', (code, signal) => {
                    const taskObj = globalThis.__clusterTasks.get(taskId);
                    taskEmitter.emit('exit', code, signal);
                    globalThis.__clusterEmitter.emit('exit', taskObj, code, signal);
                    globalThis.__clusterTasks.delete(taskId);
                });

                // Emit online on primary once message or spawn occurs
                child.once('spawn', () => {
                    const taskObj = globalThis.__clusterTasks.get(taskId);
                    taskEmitter.emit('online');
                    globalThis.__clusterEmitter.emit('online', taskObj);
                });

                return child.pid || null;
            })
        "#;

        let forker: Function<serde_json::Value, Option<u32>> = env.run_script(forker_builder)?;

        let dead_flag = Arc::new(AtomicBool::new(false));
        let exit_flag = Arc::clone(&dead_flag);

        let task = ClusterTask {
            id: task_id,
            exited_after_disconnect: Arc::new(AtomicBool::new(false)),
            child_id: task_id,
            process: Arc::new(Mutex::new(None)),
            dead: dead_flag,
            is_worker_self: false,
        };

        state.tasks.lock().unwrap().insert(task_id, task.clone_task());

        let payload = serde_json::json!({
            "taskId": task_id,
            "settings": settings,
            "customEnv": custom_env,
        });

        let pid = forker.call(payload)?;

        // Construct backing Process and attach to task
        let proc = Process::from_existing(
            CommandOptions {
                program: settings.exec.unwrap_or_else(|| "node".to_string()),
                args: settings.args,
                cwd: settings.cwd,
                env: None,
                input: None,
                timeout_ms: None,
                cleanup_timeout_ms: None,
                limits: None,
            },
            task_id,
            pid,
        );

        *task.process.lock().unwrap() = Some(proc);

        // Emit 'fork' event with taskObj
        // Register task into globalThis.__clusterTasks and emit 'fork'
        let register_and_emit_script = r#"
            (function(payload) {
                if (!globalThis.__clusterTasks) globalThis.__clusterTasks = new Map();
                return function(taskObj) {
                    globalThis.__clusterTasks.set(payload.taskId, taskObj);
                    if (globalThis.__clusterEmitter && typeof globalThis.__clusterEmitter.emit === 'function') {
                        globalThis.__clusterEmitter.emit('fork', taskObj);
                    }
                };
            })
        "#;
        let setup_reg: Function<serde_json::Value, Function<'static, ClusterTask, ()>> =
            env.run_script(register_and_emit_script)?;
        let reg_fn = setup_reg.call(serde_json::json!({ "taskId": task_id }))?;
        reg_fn.call(task.clone_task())?;

        // Hook exit to clean up from state.tasks
        let on_exit_cleanup = r#"
            (function() {
                return function(taskId, cb) {
                    const child = globalThis.__subprocesses.get(taskId);
                    if (child) {
                        child.once('exit', () => cb());
                    }
                };
            })()
        "#;
        let on_cleanup_fn: Function<Unknown, ()> = env.create_function_from_closure("cleanup", move |_| {
            exit_flag.store(true, Ordering::SeqCst);
            cluster_state().tasks.lock().unwrap().remove(&task_id);
            Ok(())
        })?;
        let hook_cleanup: Function<(u32, Function<Unknown, ()>), ()> = env.run_script(on_exit_cleanup)?;
        let _ = hook_cleanup.call((task_id, on_cleanup_fn));

        Ok(task)
    }

    /// Disconnect all tasks in cluster.
    #[napi]
    pub fn disconnect(&self, env: Env, callback: Option<Function<'static, (), ()>>) -> Result<()> {
        let tasks: Vec<ClusterTask> = {
            let guard = cluster_state().tasks.lock().unwrap();
            guard.values().map(|t| t.clone_task()).collect()
        };

        for t in &tasks {
            let _ = t.disconnect(env);
        }

        if let Some(cb) = callback {
            let cb_script = r#"
                (function(cb) {
                    process.nextTick(function() {
                        if (typeof cb === 'function') cb();
                    });
                })
            "#;
            let invoker: Function<Function<'static, (), ()>, ()> = env.run_script(cb_script)?;
            let _ = invoker.call(cb);
        }

        Ok(())
    }

    /// Hash of all active tasks, keyed by task ID.
    #[napi(getter)]
    pub fn tasks<'a>(&self, env: &'a Env) -> Result<Unknown<'a>> {
        let fetch_script = r#"
            (function() {
                const map = globalThis.__clusterTasks;
                const result = {};
                if (map) {
                    for (const [k, v] of map.entries()) {
                        result[String(k)] = v;
                    }
                }
                return result;
            })()
        "#;
        let obj: Unknown<'a> = env.run_script(fetch_script)?;
        Ok(obj)
    }

    /// Hash of all active workers (alias for `tasks`).
    #[napi(getter)]
    pub fn workers<'a>(&self, env: &'a Env) -> Result<Unknown<'a>> {
        self.tasks(env)
    }

    /// Reference to current task if running inside a worker process.
    #[napi(getter)]
    pub fn task(&self) -> Option<ClusterTask> {
        if self.is_primary() {
            return None;
        }
        let task_id: u32 = std::env::var("NODE_UNIQUE_ID")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        Some(ClusterTask {
            id: task_id,
            exited_after_disconnect: Arc::new(AtomicBool::new(false)),
            child_id: task_id,
            process: Arc::new(Mutex::new(None)),
            dead: Arc::new(AtomicBool::new(false)),
            is_worker_self: true,
        })
    }

    /// Reference to current worker (alias for `task`).
    #[napi(getter)]
    pub fn worker(&self) -> Option<ClusterTask> {
        self.task()
    }

    /// Add an event listener to the cluster module.
    #[napi]
    pub fn on(
        &self,
        env: Env,
        event: String,
        listener: Unknown,
    ) -> Result<()> {
        let script = r#"
            (function(ev) {
                if (!globalThis.__clusterEmitter) {
                    const events = (process.getBuiltinModule ? process.getBuiltinModule('node:events') : null)
                        || (typeof require === 'function' ? require('node:events') : null)
                        || (process.mainModule ? process.mainModule.require('node:events') : null);
                    globalThis.__clusterEmitter = new events.EventEmitter();
                }
                return function(cb) {
                    globalThis.__clusterEmitter.on(ev, function(...args) {
                        if (typeof cb === 'function') cb(...args);
                    });
                };
            })
        "#;
        let builder: Function<String, Function<'static, Unknown, ()>> =
            env.run_script(script)?;
        let registrator = builder.call(event)?;
        registrator.call(listener)
    }

    /// Alias for `on`.
    #[napi]
    pub fn add_event_listener(
        &self,
        env: Env,
        event: String,
        listener: Unknown,
    ) -> Result<()> {
        self.on(env, event, listener)
    }
}
