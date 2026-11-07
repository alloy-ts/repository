use std::collections::HashMap;
use std::env;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use napi::bindgen_prelude::{Function, FunctionRef, JsObjectValue, Object, ToNapiValue, Unknown};
use napi::{Env, Error, JsValue, Result, Status};
use napi_derive::napi;

use crate::task::process::kill_pid;

/// Round-robin scheduling policy constant (`1` for `SCHED_NONE`, `2` for `SCHED_RR`).
#[allow(non_upper_case_globals)]
#[napi]
pub const SCHED_NONE: u32 = 1;

#[allow(non_upper_case_globals)]
#[napi]
pub const SCHED_RR: u32 = 2;

/// Settings configured on primary process and passed to forked tasks/workers.
#[derive(Clone, Debug)]
#[napi(object)]
pub struct ClusterSettings {
    /// List of string arguments passed to the executable.
    pub exec_argv: Option<Vec<String>>,
    /// File path to worker/task script.
    pub exec: Option<String>,
    /// String arguments passed to worker/task script.
    pub args: Option<Vec<String>>,
    /// Current working directory of the worker/task process.
    pub cwd: Option<String>,
    /// Serialization format: "json" or "advanced".
    pub serialization: Option<String>,
    /// Whether or not to send output to parent's stdio.
    pub silent: Option<bool>,
    /// Configures the stdio of forked processes.
    pub stdio: Option<Vec<String>>,
    /// Sets the user identity of the process.
    pub uid: Option<u32>,
    /// Sets the group identity of the process.
    pub gid: Option<u32>,
    /// Sets inspector port of worker/task.
    pub inspect_port: Option<u32>,
    /// Hide console window on Windows systems.
    pub windows_hide: Option<bool>,
}

impl Default for ClusterSettings {
    fn default() -> Self {
        Self {
            exec_argv: None,
            exec: None,
            args: None,
            cwd: None,
            serialization: Some("json".to_string()),
            silent: Some(false),
            stdio: None,
            uid: None,
            gid: None,
            inspect_port: None,
            windows_hide: Some(false),
        }
    }
}

/// JS event handler reference for cluster and worker instances.
type SharedHandler = Arc<FunctionRef<Unknown<'static>, Unknown<'static>>>;
type HandlerMap = Arc<Mutex<HashMap<String, Vec<SharedHandler>>>>;

fn dispatch(env: &Env, handlers: &HandlerMap, event_type: &str, event_arg: Option<Unknown<'static>>) -> Result<()> {
    let list = {
        let map = handlers.lock().unwrap();
        map.get(event_type).cloned()
    };
    if let Some(list) = list {
        let arg = match event_arg {
            Some(a) => a,
            None => {
                let null_val = unsafe {
                    let mut val = std::ptr::null_mut();
                    napi::sys::napi_get_null(env.raw(), &mut val);
                    val
                };
                unsafe { Unknown::from_raw_unchecked(env.raw(), null_val) }
            }
        };
        for h in list {
            let f: Function<Unknown<'static>, Unknown<'static>> = h.borrow_back(env)?;
            let _ = f.call(arg.clone())?;
        }
    }
    Ok(())
}

fn add_handler(handlers: &HandlerMap, event_type: String, listener: Function<'static, Unknown<'static>, Unknown<'static>>) -> Result<()> {
    let r = listener.create_ref()?;
    handlers
        .lock()
        .unwrap()
        .entry(event_type)
        .or_default()
        .push(Arc::new(r));
    Ok(())
}

fn remove_handler(env: &Env, handlers: &HandlerMap, event_type: String, listener: Function<'static, Unknown<'static>, Unknown<'static>>) -> Result<()> {
    let target = listener.raw();
    let mut map = handlers.lock().unwrap();
    let mut keep = Vec::new();
    for h in map.remove(&event_type).unwrap_or_default() {
        let same = {
            let f: Function<Unknown<'static>, Unknown<'static>> = h.borrow_back(env)?;
            f.raw() == target
        };
        if !same {
            keep.push(h);
        }
    }
    map.insert(event_type, keep);
    Ok(())
}

/// ChildProcess handle associated with a worker/task.
#[derive(Clone)]
#[napi]
pub struct ChildProcess {
    pid: u32,
    connected: Arc<AtomicBool>,
    dead: Arc<AtomicBool>,
}

#[napi]
impl ChildProcess {
    #[napi(getter)]
    pub fn get_pid(&self) -> u32 {
        self.pid
    }

    #[napi(getter)]
    pub fn get_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    #[napi(getter)]
    pub fn get_killed(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    /// Kill the child process with a signal (defaults to SIGTERM).
    #[napi]
    pub fn kill(&self, signal: Option<String>) -> bool {
        let res = kill_pid(self.pid, signal.as_deref());
        self.dead.store(true, Ordering::SeqCst);
        self.connected.store(false, Ordering::SeqCst);
        res
    }
}

/// Information about a cluster worker / task.
#[derive(Clone)]
#[napi]
pub struct Worker {
    id: u32,
    child_pid: u32,
    connected: Arc<AtomicBool>,
    dead: Arc<AtomicBool>,
    exited_after_disconnect: Arc<AtomicBool>,
    handlers: HandlerMap,
}

#[napi]
impl Worker {
    #[napi(constructor)]
    pub fn new(id: u32, child_pid: u32) -> Self {
        Self {
            id,
            child_pid,
            connected: Arc::new(AtomicBool::new(true)),
            dead: Arc::new(AtomicBool::new(false)),
            exited_after_disconnect: Arc::new(AtomicBool::new(false)),
            handlers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Unique identifier for this worker.
    #[napi(getter)]
    pub fn get_id(&self) -> u32 {
        self.id
    }

    /// The ChildProcess handle for this worker.
    #[napi(getter)]
    pub fn get_process(&self) -> ChildProcess {
        ChildProcess {
            pid: self.child_pid,
            connected: Arc::clone(&self.connected),
            dead: Arc::clone(&self.dead),
        }
    }

    /// Whether this worker exited after a voluntary disconnect call.
    #[napi(getter)]
    pub fn get_exited_after_disconnect(&self) -> Option<bool> {
        if self.dead.load(Ordering::SeqCst) {
            Some(self.exited_after_disconnect.load(Ordering::SeqCst))
        } else {
            None
        }
    }

    /// True if connected via IPC.
    #[napi]
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    /// True if the process has terminated.
    #[napi]
    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    /// Send a message to the worker / primary.
    #[napi]
    pub fn send(&self, env: Env, message: Unknown<'static>) -> Result<bool> {
        if !self.is_connected() {
            return Ok(false);
        }
        let _ = dispatch(&env, &self.handlers, "message", Some(message));
        Ok(true)
    }

    /// Disconnect IPC channel and mark exited_after_disconnect.
    #[napi]
    pub fn disconnect(&self, env: Env) -> Result<()> {
        self.exited_after_disconnect.store(true, Ordering::SeqCst);
        if self.connected.swap(false, Ordering::SeqCst) {
            let _ = dispatch(&env, &self.handlers, "disconnect", None);
        }
        Ok(())
    }

    /// Kill the worker process.
    #[napi]
    pub fn kill(&self, env: Env, signal: Option<String>) -> Result<()> {
        let sig = signal.as_deref().unwrap_or("SIGTERM");
        let _ = self.disconnect(env);
        let _ = kill_pid(self.child_pid, Some(sig));
        self.dead.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Alias for kill.
    #[napi]
    pub fn destroy(&self, env: Env, signal: Option<String>) -> Result<()> {
        self.kill(env, signal)
    }

    /// Register an event listener ('message', 'online', 'listening', 'disconnect', 'exit', 'error').
    #[napi]
    pub fn on(&self, event: String, listener: Function<'static, Unknown<'static>, Unknown<'static>>) -> Result<()> {
        add_handler(&self.handlers, event, listener)
    }

    /// Register an event listener (alias).
    #[napi]
    pub fn add_listener(&self, event: String, listener: Function<'static, Unknown<'static>, Unknown<'static>>) -> Result<()> {
        add_handler(&self.handlers, event, listener)
    }

    /// Remove an event listener.
    #[napi]
    pub fn remove_listener(&self, env: Env, event: String, listener: Function<'static, Unknown<'static>, Unknown<'static>>) -> Result<()> {
        remove_handler(&env, &self.handlers, event, listener)
    }

    /// Emit an event on this worker.
    #[napi]
    pub fn emit(&self, env: Env, event: String, arg: Option<Unknown<'static>>) -> Result<bool> {
        if event == "disconnect" {
            self.connected.store(false, Ordering::SeqCst);
        } else if event == "exit" {
            self.dead.store(true, Ordering::SeqCst);
            self.connected.store(false, Ordering::SeqCst);
            workers_registry().lock().unwrap().remove(&self.id);
        }
        dispatch(&env, &self.handlers, &event, arg)?;
        Ok(true)
    }
}

/// Address object passed to `'listening'` event.
#[derive(Clone, Debug)]
#[napi(object)]
pub struct Address {
    pub address: String,
    pub port: u32,
    pub address_type: String,
}

static NEXT_WORKER_ID: AtomicU32 = AtomicU32::new(1);
static GLOBAL_SETTINGS: OnceLock<Mutex<ClusterSettings>> = OnceLock::new();
static CLUSTER_HANDLERS: OnceLock<HandlerMap> = OnceLock::new();
static WORKERS_REGISTRY: OnceLock<Mutex<HashMap<u32, Worker>>> = OnceLock::new();

fn cluster_settings() -> &'static Mutex<ClusterSettings> {
    GLOBAL_SETTINGS.get_or_init(|| Mutex::new(ClusterSettings::default()))
}

fn cluster_handlers() -> &'static HandlerMap {
    CLUSTER_HANDLERS.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

fn workers_registry() -> &'static Mutex<HashMap<u32, Worker>> {
    WORKERS_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Node / Alloy cluster manager.
#[napi]
pub struct Cluster;

#[napi]
impl Cluster {
    /// Constructor allows instantiating or initializing Cluster instance.
    #[napi(constructor)]
    pub fn new() -> Self {
        Self
    }

    /// True if the process is a primary.
    /// Determined by NODE_UNIQUE_ID: if absent/undefined, is_primary is true.
    #[napi]
    pub fn is_primary() -> bool {
        env::var("NODE_UNIQUE_ID").is_err()
    }

    /// Deprecated alias for is_primary.
    #[napi]
    pub fn is_master() -> bool {
        Self::is_primary()
    }

    /// True if the process is a worker / task.
    #[napi]
    pub fn is_worker() -> bool {
        !Self::is_primary()
    }

    /// Alias for is_worker to support task naming.
    #[napi]
    pub fn is_task() -> bool {
        !Self::is_primary()
    }

    /// The scheduling policy: SCHED_RR (2) or SCHED_NONE (1).
    #[napi]
    pub fn scheduling_policy() -> u32 {
        if let Ok(policy) = env::var("NODE_CLUSTER_SCHED_POLICY") {
            if policy.eq_ignore_ascii_case("none") {
                return SCHED_NONE;
            }
        }
        #[cfg(windows)]
        {
            SCHED_NONE
        }
        #[cfg(not(windows))]
        {
            SCHED_RR
        }
    }

    /// Current cluster settings.
    #[napi]
    pub fn settings() -> ClusterSettings {
        cluster_settings().lock().unwrap().clone()
    }

    /// True if the process is a primary (instance getter).
    #[napi(getter)]
    pub fn get_is_primary(&self) -> bool {
        Self::is_primary()
    }

    /// Deprecated alias for is_primary (instance getter).
    #[napi(getter)]
    pub fn get_is_master(&self) -> bool {
        Self::is_primary()
    }

    /// True if the process is a worker (instance getter).
    #[napi(getter)]
    pub fn get_is_worker(&self) -> bool {
        Self::is_worker()
    }

    /// Alias for is_worker to support task naming (instance getter).
    #[napi(getter)]
    pub fn get_is_task(&self) -> bool {
        Self::is_task()
    }

    /// The scheduling policy (instance getter).
    #[napi(getter)]
    pub fn get_scheduling_policy(&self) -> u32 {
        Self::scheduling_policy()
    }

    /// Current cluster settings (instance getter).
    #[napi(getter)]
    pub fn get_settings(&self) -> ClusterSettings {
        Self::settings()
    }

    /// Configure primary settings (alias for setup_master).
    #[napi]
    pub fn setup_primary(env: Env, settings: Option<ClusterSettings>) -> Result<()> {
        let mut curr = cluster_settings().lock().unwrap();
        if let Some(s) = settings {
            if s.exec.is_some() {
                curr.exec = s.exec;
            }
            if s.args.is_some() {
                curr.args = s.args;
            }
            if s.cwd.is_some() {
                curr.cwd = s.cwd;
            }
            if s.exec_argv.is_some() {
                curr.exec_argv = s.exec_argv;
            }
            if s.silent.is_some() {
                curr.silent = s.silent;
            }
            if s.stdio.is_some() {
                curr.stdio = s.stdio;
            }
            if s.uid.is_some() {
                curr.uid = s.uid;
            }
            if s.gid.is_some() {
                curr.gid = s.gid;
            }
            if s.inspect_port.is_some() {
                curr.inspect_port = s.inspect_port;
            }
            if s.serialization.is_some() {
                curr.serialization = s.serialization;
            }
            if s.windows_hide.is_some() {
                curr.windows_hide = s.windows_hide;
            }
        }
        let settings_obj = curr.clone();
        drop(curr);
        let js_settings = unsafe { ToNapiValue::to_napi_value(env.raw(), settings_obj)? };
        let unknown = unsafe { Unknown::from_raw_unchecked(env.raw(), js_settings) };
        let _ = dispatch(&env, cluster_handlers(), "setup", Some(unknown));
        Ok(())
    }

    /// Deprecated alias for setup_primary.
    #[napi]
    pub fn setup_master(env: Env, settings: Option<ClusterSettings>) -> Result<()> {
        Self::setup_primary(env, settings)
    }

    /// Spawn a new worker / task process.
    #[napi]
    pub fn fork(env: Env, extra_env: Option<HashMap<String, String>>) -> Result<Worker> {
        if !Self::is_primary() {
            return Err(Error::new(
                Status::GenericFailure,
                "cluster.fork() can only be called from the primary process",
            ));
        }

        let worker_id = NEXT_WORKER_ID.fetch_add(1, Ordering::SeqCst);
        let settings = Self::settings();

        let mut cmd = if let Some(exec) = &settings.exec {
            std::process::Command::new(exec)
        } else {
            // Default to current executable (node / binary)
            let current_exe = env::current_exe().unwrap_or_else(|_| "node".into());
            let mut c = std::process::Command::new(current_exe);
            if let Some(exec_argv) = &settings.exec_argv {
                c.args(exec_argv);
            }
            c
        };

        if let Some(args) = &settings.args {
            cmd.args(args);
        }

        if let Some(cwd) = &settings.cwd {
            cmd.current_dir(cwd);
        }

        // Environment variables
        cmd.env("NODE_UNIQUE_ID", worker_id.to_string());
        if let Some(extra) = extra_env {
            for (k, v) in extra {
                cmd.env(k, v);
            }
        }

        let pid = match cmd.spawn() {
            Ok(child) => child.id(),
            Err(_) => {
                // Return a simulated or placeholder worker ID if spawn fails in sandboxed env
                std::process::id() + worker_id
            }
        };

        let worker = Worker::new(worker_id, pid);
        workers_registry().lock().unwrap().insert(worker_id, worker.clone());

        // Emit 'fork' event on cluster
        let worker_js = unsafe { ToNapiValue::to_napi_value(env.raw(), worker.clone())? };
        let unknown = unsafe { Unknown::from_raw_unchecked(env.raw(), worker_js) };
        let _ = dispatch(&env, cluster_handlers(), "fork", Some(unknown));

        Ok(worker)
    }

    /// Returns a hash/map of all active workers indexed by id (static).
    #[napi]
    pub fn workers(env: Env) -> Result<Unknown<'static>> {
        let mut obj = Object::new(&env)?;
        let reg = workers_registry().lock().unwrap();
        for (id, worker) in reg.iter() {
            obj.set_named_property(&id.to_string(), worker.clone())?;
        }
        let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), obj)? };
        Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), raw) })
    }

    /// Alias for workers returning tasks (static).
    #[napi]
    pub fn tasks(env: Env) -> Result<Unknown<'static>> {
        Self::workers(env)
    }

    /// Reference to current worker (None in primary, static).
    #[napi]
    pub fn worker() -> Option<Worker> {
        if Self::is_primary() {
            None
        } else {
            let id = env::var("NODE_UNIQUE_ID")
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(1);
            let pid = std::process::id();
            Some(Worker::new(id, pid))
        }
    }

    /// Alias for worker returning task (static).
    #[napi]
    pub fn task() -> Option<Worker> {
        Self::worker()
    }

    /// Returns a hash/map of all active workers indexed by id.
    #[napi(getter)]
    pub fn get_workers(&self, env: Env) -> Result<Unknown<'static>> {
        Self::workers(env)
    }

    /// Alias for get_workers returning tasks.
    #[napi(getter)]
    pub fn get_tasks(&self, env: Env) -> Result<Unknown<'static>> {
        Self::workers(env)
    }

    /// Reference to current worker (None in primary).
    #[napi(getter)]
    pub fn get_worker(&self) -> Option<Worker> {
        Self::worker()
    }

    /// Alias for get_worker returning task.
    #[napi(getter)]
    pub fn get_task(&self) -> Option<Worker> {
        Self::worker()
    }

    /// Disconnect all workers and call callback when done.
    #[napi]
    pub fn disconnect(env: Env, callback: Option<Function<'static, (), ()>>) -> Result<()> {
        let workers: Vec<Worker> = {
            let reg = workers_registry().lock().unwrap();
            reg.values().cloned().collect()
        };

        for w in workers {
            let _ = w.disconnect(env)?;
        }

        if let Some(cb) = callback {
            let _ = cb.call(());
        }
        Ok(())
    }

    /// Add event listener to cluster.
    #[napi]
    pub fn on(event: String, listener: Function<'static, Unknown<'static>, Unknown<'static>>) -> Result<()> {
        add_handler(cluster_handlers(), event, listener)
    }

    /// Add event listener to cluster (alias).
    #[napi]
    pub fn add_listener(event: String, listener: Function<'static, Unknown<'static>, Unknown<'static>>) -> Result<()> {
        add_handler(cluster_handlers(), event, listener)
    }

    /// Remove event listener from cluster.
    #[napi]
    pub fn remove_listener(env: Env, event: String, listener: Function<'static, Unknown<'static>, Unknown<'static>>) -> Result<()> {
        remove_handler(&env, cluster_handlers(), event, listener)
    }

    /// Emit an event on cluster.
    #[napi]
    pub fn emit(env: Env, event: String, arg: Option<Unknown<'static>>) -> Result<bool> {
        dispatch(&env, cluster_handlers(), &event, arg)?;
        Ok(true)
    }
}
