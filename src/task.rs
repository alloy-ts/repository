use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use napi::bindgen_prelude::{
    ClassInstance, Either, FromNapiValue, Function, FunctionRef, JsObjectValue, JsValue, Object,
    ToNapiValue, Unknown,
};
use napi::threadsafe_function::ThreadsafeFunctionCallMode;
use napi::{Env, Error, JsDeferred, Result, Status};
use napi_derive::napi;

use libuv::{AsyncHandle, HandleTrait, Loop, ReqTrait, WorkReq as TaskReq};

use crate::error::{err, thread_local};
use crate::repository::Repository;
use crate::thread::ThreadPool;
use gix::objs::Find;

/// `1024 * 1024`. Mirrors the C++ `task` binding's `kMB` constant, used to
/// convert resource-limit megabytes to bytes.
#[allow(non_upper_case_globals)]
#[napi]
pub const kMB: u32 = 1024 * 1024;

/// Resource-limit array indices, mirroring the C++ `task` binding's
/// `kMaxYoungGenerationSizeMb` / `kMaxOldGenerationSizeMb` / `kCodeRangeSizeMb`
/// / `kStackSizeMb` constants. The task receives them as a `Float64Array`.
#[allow(non_upper_case_globals)]
#[napi]
pub const kMaxYoungGenerationSizeMb: u32 = 0;
#[allow(non_upper_case_globals)]
#[napi]
pub const kMaxOldGenerationSizeMb: u32 = 1;
#[allow(non_upper_case_globals)]
#[napi]
pub const kCodeRangeSizeMb: u32 = 2;
#[allow(non_upper_case_globals)]
#[napi]
pub const kStackSizeMb: u32 = 3;
#[allow(non_upper_case_globals)]
#[napi]
pub const kTotalResourceLimitCount: u32 = 4;

/// V8 resource limits for a task, mirroring the C++ `Task` `resource_limits_`
/// float array (`node_task.h`): `maxYoungGenerationSizeMb`,
/// `maxOldGenerationSizeMb`, `codeRangeSizeMb`, `stackSizeMb`. `None` means
/// "inherit Node's default".
#[derive(Clone)]
#[napi(object)]
pub struct TaskResourceLimits {
    #[napi]
    pub max_young_generation_size_mb: Option<u32>,
    #[napi]
    pub max_old_generation_size_mb: Option<u32>,
    #[napi]
    pub code_range_size_mb: Option<u32>,
    #[napi]
    pub stack_size_mb: Option<u32>,
}

/// Options for constructing a [`Task`].
///
/// Every constructor argument lives here: `new Task(options)`.
#[napi(object)]
pub struct TaskOptions {
    /// The task the task runs. It may be:
    ///
    ///   * inline code text,
    ///   * a git object id naming a blob in the object database (the blob's
    ///     contents are used),
    ///   * a filesystem path or `file://` URL to a script,
    ///   * a filesystem path or `file://` URL to a native `.node` addon, a
    ///     `.wasm` module, or another executable module format,
    ///   * an `http(s)` URL, which is fetched first (see `allowNettask`).
    ///
    /// A single entry, or a list of entries. Entries run in order in the
    /// task's scope (sharing `postMessage` / `onMessage` / `taskData`); the
    /// first failure stops the sequence with exit code 1.
    #[napi]
    pub entries: Either<String, Vec<String>>,
    /// Tasktree id (a directory name under `.git/tasktrees/`) or a filesystem
    /// path to an existing tasktree — either the main tasking tree or a linked
    /// tasktree. Ignored when no tasktree matches, falling back to `repository`
    /// and then the environment.
    ///
    /// Pass `"."` to run against the real current tasking directory. That
    /// explicitly opts out of the per-task temp tasktree and takes precedence
    /// over `sandbox`.
    #[napi]
    pub tasktree_path: Option<String>,
    /// Run the task isolated in its own tasktree. When `true` and
    /// `tasktreePath` is omitted, a tasktree is created from the current HEAD at
    /// `${os.tmpdir()}/${taskId}`; when `tasktreePath` is given, that location is
    /// used instead. Each task then owns a private directory, so parallel tasks
    /// cannot collide on the filesystem.
    #[napi]
    pub sandbox: Option<bool>,
    /// Optional name for the task (surfaced through `Task.threadName`).
    #[napi]
    pub name: Option<String>,
    /// V8 resource limits (heap / code-range / stack size).
    #[napi]
    pub resource_limits: Option<TaskResourceLimits>,
    /// Per-task `process.env`. Accepts `'inherit'`, `'none'`, or a plain object.
    ///
    /// A libuv-backed task shares the process environment, so this is recorded
    /// but not applied.
    #[napi]
    pub env: Option<serde_json::Value>,
    /// Extra CLI args recorded for the task.
    #[napi]
    pub argv: Option<Vec<String>>,
    /// Extra Node CLI args recorded for the task.
    #[napi]
    pub exec_argv: Option<Vec<String>>,
    /// Arbitrary data handed to the task as `taskData`.
    #[napi]
    pub task_data: Option<serde_json::Value>,
    /// Forward unmanaged file descriptors into the task.
    #[napi]
    pub track_unmanaged_fds: Option<bool>,
    /// Permit fetching `task` from a remote `http(s)` URL. Off by default:
    /// network access is never implicit. Local paths and `file://` URLs need no
    /// permission.
    #[napi]
    pub allow_network: Option<bool>,
    /// Repository used to resolve `task` when it is a git blob OID. If
    /// `task` parses as an object id and exists as a blob in this
    /// repository, its contents are read and executed as code; otherwise
    /// `task` is treated as inline code text. When omitted, the repository
    /// is resolved from the environment (`GIT_DIR` / `REPO_DIR`, else discovery
    /// from the current directory); if none can be resolved, `task` is
    /// treated as inline code.
    #[napi]
    pub repository: Option<ClassInstance<'static, Repository>>,
}

// ---------------------------------------------------------------------------
// Programme resolution
// ---------------------------------------------------------------------------

/// Module extensions handed to the runtime as a module path rather than as code.
const LOADABLE_EXTS: [&str; 5] = ["js", "cjs", "mjs", "node", "json"];

/// What `task` resolved to.
enum Programme {
    /// Code text, evaluated on the JS thread.
    Code(String),
    /// A module: its path plus its source, read on the task thread so the JS
    /// thread never blocks on file I/O. The source is inlined into the runner
    /// wrapper, so `postMessage` / `onMessage` / `taskData` are lexically in
    /// scope inside the module — no globals are published.
    Module { path: String, source: String },
}

impl Programme {
    fn is_code(&self) -> bool {
        matches!(self, Programme::Code(_))
    }
}

/// Whether `spec` names the current tasking directory (the real filesystem the
/// process is running in), as opposed to a per-task temp tasktree.
fn is_cwd(spec: &str) -> bool {
    spec == "." || spec == "./"
}

/// Whether `s` carries a URL scheme (e.g. `http://`, `file://`).
fn has_scheme(s: &str) -> bool {
    match s.find("://") {
        Some(i) if i > 0 => s[..i]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')),
        _ => false,
    }
}

/// Lowercased file extension of `s`, if any.
fn ext(s: &str) -> Option<String> {
    Path::new(s)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
}

/// Code that instantiates a `.wasm` module and calls its `_start` export
/// (WASI-style). `.wasm` is not a CommonJS module, so it is loaded from
/// generated code.
fn wasm_start(path: &str) -> String {
    let literal = format!("{path:?}");
    format!(
        r#"const fs = require('node:fs');
const bytes = fs.readFileSync({literal});
WebAssembly.instantiate(bytes, {{}}).then(({{ instance }}) => {{
  const start = instance.exports._start;
  if (typeof start !== 'function') throw new Error('wasm module has no _start export');
  start();
}}).catch((e) => process.nextTick(() => {{ throw e; }}));
"#
    )
}

/// Download `url` into `dir` (the sandbox tasktree when sandboxing, else a fresh
/// directory under the OS temp directory) and return the local path.
///
/// Shells out to `curl`, mirroring how the rest of this crate shells out to
/// `git`: no general-purpose HTTP client is in the dependency tree.
fn fetch_to_temp(url: &str, dir: Option<&Path>) -> Result<String> {
    let dir = match dir {
        Some(d) => d.to_path_buf(),
        None => std::env::temp_dir().join(format!("task-task-{}", uuid::Uuid::new_v4())),
    };
    std::fs::create_dir_all(&dir).map_err(|e| {
        Error::new(
            Status::GenericFailure,
            format!("failed to create {}: {e}", dir.display()),
        )
    })?;
    let name = url
        .rsplit('/')
        .next()
        .filter(|n| !n.is_empty())
        .unwrap_or("task");
    let dest = dir.join(name);
    let out = std::process::Command::new("curl")
        .args(["-fsSL", url, "-o"])
        .arg(&dest)
        .output()
        .map_err(|e| Error::new(Status::GenericFailure, format!("failed to run curl: {e}")))?;
    if !out.status.success() {
        return Err(Error::new(
            Status::GenericFailure,
            format!(
                "curl failed to fetch {url}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ));
    }
    Ok(dest.to_string_lossy().into_owned())
}

/// If `task` names a module file, return the local path to load. Handles
/// `file://` URLs, remote `http(s)` URLs (only when permitted) and plain
/// filesystem paths. Returns `None` when it is not a module reference.
fn module_file(
    task: &str,
    sandbox_dir: Option<&Path>,
    allow_network: bool,
) -> Result<Option<String>> {
    if has_scheme(task) {
        if let Some(rest) = task.strip_prefix("file://") {
            return Ok(Some(rest.to_string()));
        }
        if task.starts_with("http://") || task.starts_with("https://") {
            if !allow_network {
                return Err(Error::new(
                    Status::GenericFailure,
                    format!(
                        "fetching `task` over the network requires allowNettask: true ({task})"
                    ),
                ));
            }
            return fetch_to_temp(task, sandbox_dir).map(Some);
        }
        return Ok(None);
    }
    let looks_like_module = Path::new(task).is_file()
        || matches!(ext(task), Some(e) if LOADABLE_EXTS.contains(&e.as_str()) || e == "wasm");
    if looks_like_module {
        Ok(Some(task.to_string()))
    } else {
        Ok(None)
    }
}

/// Resolve `task` into something the runtime can start, in order:
///
/// 1. a git blob OID present in the object database → its contents as code text,
/// 2. a `file://` / `http(s)` URL or filesystem path to a module → that local
///    path (`.wasm` is instantiated and `_start`ed from generated code),
/// 3. anything else → inline code text.
///
/// Runs on a libuv task thread: it only touches the filesystem, the object
/// database and (optionally) the network, never V8.
fn resolve_task(
    task: &str,
    repo: Option<&Arc<gix::ThreadSafeRepository>>,
    sandbox_dir: Option<&Path>,
    allow_network: bool,
) -> Result<Programme> {
    if let (Ok(oid), Some(inner)) = (gix::ObjectId::from_hex(task.as_bytes()), repo) {
        let r = thread_local(inner);
        let mut buf = Vec::new();
        if let Some(d) = Find::try_find(&r.objects, &oid, &mut buf)
            .map_err(|e| Error::new(Status::GenericFailure, format!("try_find {oid}: {e}")))?
        {
            if d.kind == gix::object::Kind::Blob {
                let code = String::from_utf8(d.data.to_vec()).map_err(|e| {
                    Error::new(
                        Status::GenericFailure,
                        format!("task blob {oid} is not valid UTF-8: {e}"),
                    )
                })?;
                return Ok(Programme::Code(code));
            }
        }
    }
    if let Some(path) = module_file(task, sandbox_dir, allow_network)? {
        if ext(&path) == Some("wasm".to_string()) {
            return Ok(Programme::Code(wasm_start(&path)));
        }
        let source = std::fs::read_to_string(&path).map_err(|e| {
            Error::new(
                Status::GenericFailure,
                format!("failed to read task {path}: {e}"),
            )
        })?;
        return Ok(Programme::Module { path, source });
    }
    Ok(Programme::Code(task.to_string()))
}

// ---------------------------------------------------------------------------
// Repository / tasktree resolution
// ---------------------------------------------------------------------------

/// Create a linked tasktree at `dir` from the current HEAD and open it as a
/// repository.
///
/// The tasktree is created with a *detached* HEAD: a branch cannot be checked
/// out in two tasktrees at once, so detaching is what lets any number of
/// sandboxed tasks run in parallel. Linked tasktrees share the main
/// repository's object database, so creating one copies no objects.
fn create_sandboxed_tasktree(dir: &Path) -> Result<Arc<gix::ThreadSafeRepository>> {
    let base = crate::repository::current_repo()?;
    let r = thread_local(&base);
    let head = r.head_id().map_err(|e| err("head_id", e))?.detach();
    let (added, _outcome) = r
        .add_worktree(
            dir,
            gix::worktree::add::Head::Detached(head),
            gix::progress::Discard,
            &AtomicBool::new(false),
        )
        .map_err(|e| err("add_tasktree", e))?;
    // Reopen the administrative directory to obtain an owned, thread-safe handle.
    let git_dir = added.git_dir().to_path_buf();
    let ts = gix::ThreadSafeRepository::open(&git_dir).map_err(|e| err("tasktree_open", e))?;
    Ok(Arc::new(ts))
}

/// Resolve `spec` — a tasktree id (a directory name under `$GIT_DIR/tasktrees/`)
/// or a filesystem path to a tasktree checkout — to its repository. Returns
/// `None` when no tasktree matches.
fn repo_from_tasktree(
    spec: &str,
    repo: Option<&ClassInstance<'static, Repository>>,
) -> Option<Arc<gix::ThreadSafeRepository>> {
    let base = match repo {
        Some(r) => Arc::clone(&r.inner),
        None => crate::repository::current_repo().ok()?,
    };
    // 1. A registered tasktree id: `$GIT_DIR/tasktrees/<id>`.
    {
        let r = thread_local(&base);
        if let Ok(Some(proxy)) = r.worktree_proxy_by_id(spec) {
            if let Ok(ts) = gix::ThreadSafeRepository::open(proxy.git_dir()) {
                return Some(Arc::new(ts));
            }
        }
    }
    // 2. A filesystem path to a checkout — the main tasking tree or a linked one.
    let path = Path::new(spec);
    if path.is_dir() {
        if let Ok(r) = gix::discover(path) {
            return Some(Arc::new(r.into()));
        }
    }
    None
}

/// Resolve the repository the task runs against, in priority order:
///
/// 1. a sandboxed tasktree created for this task (`sandbox`),
/// 2. an existing tasktree named by `tasktreePath` (id or filesystem path),
/// 3. the explicitly supplied `repository`,
/// 4. the environment (`GIT_DIR` / `REPO_DIR`, else discovery from cwd).
///
/// Returns `None` when nothing can be resolved, in which case `task` is run
/// as inline code text.
fn resolve_repo(
    options: &TaskOptions,
    sandbox_dir: Option<&Path>,
) -> Option<Arc<gix::ThreadSafeRepository>> {
    if let Some(dir) = sandbox_dir {
        if let Ok(repo) = create_sandboxed_tasktree(dir) {
            return Some(repo);
        }
    }
    if let Some(spec) = options.tasktree_path.as_deref() {
        if let Some(repo) = repo_from_tasktree(spec, options.repository.as_ref()) {
            return Some(repo);
        }
    }
    if let Some(r) = options.repository.as_ref() {
        return Some(Arc::clone(&r.inner));
    }
    crate::repository::current_repo().ok()
}

/// Directory of the isolated tasktree, when sandboxing. `.` explicitly selects
/// the real tasking directory, overriding the per-task temp tasktree.
fn sandbox_dir(options: &TaskOptions, task_id: u32) -> Option<PathBuf> {
    match options.tasktree_path.as_deref() {
        Some(p) if is_cwd(p) => None,
        Some(p) if options.sandbox.unwrap_or(false) => Some(PathBuf::from(p)),
        None if options.sandbox.unwrap_or(false) => {
            Some(std::env::temp_dir().join(task_id.to_string()))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Native event handling
// ---------------------------------------------------------------------------

/// A JS event handler, kept alive by the task rather than by `globalThis`.
///
/// [`FunctionRef`] holds a `napi_ref` to the function; `borrow_back` yields a
/// callable [`Function`] on the JS thread. It is `Send + Sync`, so it can be
/// moved into the libuv after-task callback.
type SharedHandler = Arc<FunctionRef<Unknown<'static>, Unknown<'static>>>;

/// Handlers registered for a task: the `on*` properties plus `addEventListener`
/// listeners, keyed by event type.
type HandlerMap = Arc<Mutex<HashMap<String, Vec<SharedHandler>>>>;

/// Resolver used to finish a task on the JS thread once the libuv after-task
/// callback fires.
type DeferredResolver = Box<dyn FnOnce(Env) -> Result<()> + Send>;

/// Resolver used to settle [`Task::terminate`]'s promise with the exit code.
type DeferredU32 = Box<dyn FnOnce(Env) -> Result<u32> + Send>;

/// Termination state shared between `terminate()` and the exit path: the last
/// recorded exit code and a pending `terminate()` deferred, if any.
#[derive(Default)]
struct Termination {
    code: Mutex<Option<u32>>,
    pending: Mutex<Option<JsDeferred<u32, DeferredU32>>>,
}

type TerminateSlot = Arc<Termination>;

/// Invoke every handler registered for `type_` with `event`.
fn dispatch(env: &Env, handlers: &HandlerMap, type_: &str, event: &Unknown<'static>) -> Result<()> {
    for h in handlers.lock().unwrap().get(type_).into_iter().flatten() {
        let f: Function<Unknown<'static>, Unknown<'static>> = h.borrow_back(env)?;
        let _ = f.call(event.clone())?;
    }
    Ok(())
}

/// Emit a terminal event (`{ type, code }`).
fn emit_code(env: &Env, handlers: &HandlerMap, type_: &str, code: u32) -> Result<()> {
    let mut o = Object::new(env)?;
    o.set_named_property("type", type_)?;
    o.set_named_property("code", code)?;
    let evt = unsafe { Unknown::from_raw_unchecked(env.raw(), o.raw()) };
    dispatch(env, handlers, type_, &evt)
}

/// Emit a payload event (`{ type, data }`).
fn emit_data(env: &Env, handlers: &HandlerMap, type_: &str, data: Unknown<'static>) -> Result<()> {
    let mut o = Object::new(env)?;
    o.set_named_property("type", type_)?;
    o.set_named_property("data", data.raw())?;
    let evt = unsafe { Unknown::from_raw_unchecked(env.raw(), o.raw()) };
    dispatch(env, handlers, type_, &evt)
}

/// Turn any `ToNapiValue` into an `Unknown`.
fn to_unknown<T: ToNapiValue>(env: &Env, v: T) -> Result<Unknown<'static>> {
    let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), v)? };
    Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), raw) })
}

// ---------------------------------------------------------------------------
// The `Task` class
// ---------------------------------------------------------------------------

/// Monotonic, process-unique id for each [`Task`] instance.
static NEXT_TASK_ID: AtomicU32 = AtomicU32::new(1);

/// A libuv `uv_queue_task` task that presents a Node-Executor-compatible API.
#[napi]
pub struct Task {
    /// Process-unique id.
    id: u32,
    /// A stable, task-like id for the task.
    thread_id: u32,
    /// The task's name (from construction options).
    name: String,
    /// Whether the task keeps the process alive (`true` by default).
    has_ref: Arc<AtomicBool>,
    /// The resource limits the task was created with.
    resource_limits: Option<TaskResourceLimits>,
    /// Whether [`Task::terminate`] has been called. Shared with the task
    /// callback so a terminated task stops before it evaluates its task.
    terminated: Arc<AtomicBool>,
    /// JS event handlers and listeners, held natively.
    handlers: HandlerMap,
    /// `taskData`, exposed to the task.
    task_data: Option<serde_json::Value>,
    /// Raw JS promise settled once the task finishes (see [`Task::done`]).
    done: napi::sys::napi_value,
    /// Deferred created by [`Task::terminate`], settled when `exit` is emitted.
    terminate_slot: TerminateSlot,
    /// The underlying `uv_task_t` request, kept so [`Task::cancel`] and
    /// [`Task::terminate`] can issue `uv_cancel` before the task starts.
    task_req: Mutex<Option<TaskReq>>,
}

// `task_req` holds a libuv `TaskReq` (`!Send`/`!Sync`) and `done` an env-bound
// `napi_value`, but a `Task` is only ever created, used and dropped on the JS
// thread, so marking it `Send`/`Sync` is sound here.
unsafe impl Send for Task {}
unsafe impl Sync for Task {}

#[napi]
impl Task {
    /// Construct a new task that runs `options.task`.
    ///
    /// The task is resolved and the task is queued onto libuv's thread pool
    /// immediately, mirroring how `node:task_threads` starts a task on
    /// construction. Blocking setup (sandbox tasktree creation, object-database
    /// reads, network fetches) runs on a pool thread; the task itself is
    /// evaluated on the JS thread in the after-task callback.
    #[napi(constructor)]
    pub fn new(env: Env, options: TaskOptions) -> Result<Self> {
        let id = NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed);
        let thread_id = id;
        let name = options.name.clone().unwrap_or_default();
        let resource_limits = options.resource_limits.clone();
        let task_data = options.task_data.clone();

        let dir = sandbox_dir(&options, id);
        let allow_network = options.allow_network.unwrap_or(false);

        let handlers: HandlerMap = Arc::new(Mutex::new(HashMap::new()));
        let terminated = Arc::new(AtomicBool::new(false));
        let has_ref = Arc::new(AtomicBool::new(true));
        let terminate_slot: TerminateSlot = Arc::new(Termination::default());

        let (deferred, promise) = env
            .create_deferred::<(), DeferredResolver>()
            .map_err(|e| Error::new(Status::GenericFailure, format!("create_deferred: {e}")))?;
        // Keep the raw promise so JS can `await task.done()`; the deferred is
        // settled by the after-task path once the task has run.
        let done = unsafe { ToNapiValue::to_napi_value(env.raw(), promise)? };
        let deferred_slot: Arc<Mutex<Option<JsDeferred<(), DeferredResolver>>>> =
            Arc::new(Mutex::new(Some(deferred)));

        let pool = ThreadPool::new()?;

        // `task` runs on a libuv task thread (no V8 access): it resolves the
        // task plus the sandbox/object-database/network setup. The result
        // is parked in a shared slot and delivered to the loop thread through
        // the `Task` (uv_async_t) wakeup used internally by `ThreadPool::exec`.
        let task_dir = dir.clone();
        let task_options_repo = options.repository.as_ref().map(|r| Arc::clone(&r.inner));
        let task_entries: Vec<String> = match &options.entries {
            Either::A(entry) => vec![entry.clone()],
            Either::B(entries) => entries.clone(),
        };
        let task_tasktree = options.tasktree_path.clone();
        let task_sandbox = options.sandbox;
        let task = move || -> std::result::Result<Vec<Programme>, String> {
            // Resolving the sandbox tasktree needs the same priority order as
            // `resolve_repo`, so reconstruct just enough of the options.
            let probe = TaskOptions {
                entries: Either::A(String::new()),
                tasktree_path: task_tasktree.clone(),
                sandbox: task_sandbox,
                name: None,
                resource_limits: None,
                env: None,
                argv: None,
                exec_argv: None,
                task_data: None,
                track_unmanaged_fds: None,
                allow_network: None,
                repository: None,
            };
            let repo = if let Some(d) = task_dir.as_deref() {
                create_sandboxed_tasktree(d)
                    .ok()
                    .or_else(|| task_options_repo.as_ref().map(Arc::clone))
            } else {
                resolve_repo(&probe, None).or_else(|| task_options_repo.as_ref().map(Arc::clone))
            };
            // Resolve every entry up front (still off the JS thread); the
            // entries are then evaluated in order on the JS thread.
            task_entries
                .iter()
                .map(|entry| {
                    resolve_task(entry, repo.as_ref(), task_dir.as_deref(), allow_network)
                        .map_err(|e| e.reason)
                })
                .collect()
        };

        // `on_loop` runs on the event-loop thread (via the `Task` uv_async_t
        // wakeup): settle the deferred, which evaluates the task and
        // dispatches events.
        let deliver_deferred = Arc::clone(&deferred_slot);
        let deliver_handlers = Arc::clone(&handlers);
        let deliver_terminated = Arc::clone(&terminated);
        let deliver_task_data = task_data.clone();
        let deliver_terminate = Arc::clone(&terminate_slot);
        let on_loop = move |resolved: std::result::Result<Vec<Programme>, String>,
                            cancelled: bool| {
            // A cancelled after-task status means `uv_cancel` succeeded and
            // the task never ran: report it as an error so `exit`/`done()`
            // still settle.
            let resolved = if cancelled {
                Err("task was cancelled".to_string())
            } else {
                resolved
            };
            if let Some(d) = deliver_deferred.lock().unwrap().take() {
                let handlers = Arc::clone(&deliver_handlers);
                let terminated = Arc::clone(&deliver_terminated);
                let terminate_slot = Arc::clone(&deliver_terminate);
                let task_data = deliver_task_data.clone();
                d.resolve(Box::new(move |env: Env| {
                    run_resolved(
                        &env,
                        &handlers,
                        &terminated,
                        Some(resolved),
                        task_data,
                        &terminate_slot,
                    )
                }));
            }
        };

        let task_req = pool
            .exec_then(task, on_loop)
            .map_err(|e| Error::new(Status::GenericFailure, format!("exec: {e}")))?;

        Ok(Task {
            id,
            thread_id,
            name,
            has_ref,
            resource_limits,
            terminated,
            handlers,
            task_data,
            done,
            terminate_slot,
            task_req: Mutex::new(Some(task_req)),
        })
    }

    /// The task's task-like id. Mirrors `Executor.threadId`.
    #[napi(getter)]
    pub fn get_thread_id(&self) -> u32 {
        self.thread_id
    }

    /// Whether [`Executor::terminate`] has been called.
    #[napi(getter)]
    pub fn get_terminated(&self) -> bool {
        self.terminated.load(Ordering::Relaxed)
    }

    /// Whether the task keeps the process alive (`true` by default). Mirrors
    /// `Executor.hasRef`.
    #[napi(getter)]
    pub fn get_has_ref(&self) -> bool {
        self.has_ref.load(Ordering::Relaxed)
    }

    /// The task's name (from construction options).
    #[napi(getter)]
    pub fn get_thread_name(&self) -> String {
        self.name.clone()
    }

    /// The resource limits the task was created with.
    #[napi(getter)]
    pub fn get_resource_limits(&self) -> Option<TaskResourceLimits> {
        self.resource_limits.clone()
    }

    /// `taskData` handed to the task.
    #[napi(getter)]
    pub fn get_task_data(&self, env: Env) -> Result<Unknown<'static>> {
        match &self.task_data {
            Some(v) => to_unknown(&env, v.clone()),
            None => to_unknown(&env, ()),
        }
    }

    /// Deliver `message` to the task's `onMessage` handler, if it
    /// registered one.
    #[napi]
    pub fn post_message(&self, env: Env, message: Unknown<'static>) -> Result<()> {
        let mut boxed: Option<Function<Unknown<'static>, Unknown<'static>>> = None;
        {
            let map = self.handlers.lock().unwrap();
            if let Some(list) = map.get("__inbox") {
                if let Some(h) = list.first() {
                    boxed = Some(h.borrow_back(&env)?);
                }
            }
        }
        if let Some(f) = boxed {
            let _ = f.call(message)?;
        }
        Ok(())
    }

    /// Stop the task. Resolves to the task's exit code once `exit` has been
    /// emitted, mirroring `tasker.terminate()` (which resolves when the thread
    /// has actually stopped).
    #[napi(ts_return_type = "Promise<number>")]
    pub fn terminate(&self, env: Env) -> Result<Unknown<'static>> {
        self.terminated.store(true, Ordering::Relaxed);
        // If the task already exited, resolve with the recorded exit code
        // (mirrors Node, where terminating a stopped tasker resolves at once).
        if let Some(code) = *self.terminate_slot.code.lock().unwrap() {
            return to_unknown(&env, code);
        }
        // `uv_cancel` the pending request; a no-op (reported as an error we
        // ignore) once the task has started or completed.
        self.uv_cancel();
        let (deferred, promise) = env
            .create_deferred::<u32, DeferredU32>()
            .map_err(|e| Error::new(Status::GenericFailure, format!("create_deferred: {e}")))?;
        *self.terminate_slot.pending.lock().unwrap() = Some(deferred);
        let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), promise)? };
        Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), raw) })
    }

    /// `uv_cancel` on the underlying `uv_work_t` request. Returns `true` if
    /// the request was cancelled before its work started — the after-work
    /// callback then runs with status `UV_ECANCELED` and the task emits
    /// `error` + `exit` with code 1. Returns `false` once the work has started
    /// or completed (use [`Task::terminate`] for cooperative termination).
    #[napi]
    pub fn cancel(&self) -> Result<bool> {
        Ok(self.uv_cancel())
    }

    /// Rust-side `uv_cancel` over the stored request; takes it so the cancel
    /// is issued at most once. Safe to call on an already-completed request:
    /// libuv reports `UV_EALREADY` and leaves the request alone.
    fn uv_cancel(&self) -> bool {
        match self.task_req.lock().unwrap().take() {
            Some(req) => {
                let mut r: libuv::Req = libuv::Req::from(req);
                matches!(r.cancel(), Ok(_))
            }
            None => false,
        }
    }

    /// A `Promise` that settles once the task's task has finished running
    /// and `exit` has been emitted: it resolves if the task ran (or was
    /// skipped after `terminate`), and rejects if the task could not be
    /// resolved or evaluated. This is the promise-based counterpart to the
    /// `exit` event.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn done(&self, env: Env) -> Result<Unknown<'static>> {
        Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), self.done) })
    }

    /// Keep the process alive while the task is running (default).
    #[napi]
    pub fn reference(&self) -> Result<()> {
        self.has_ref.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Let the process exit even while the task is running.
    #[napi]
    pub fn unreference(&self) -> Result<()> {
        self.has_ref.store(false, Ordering::Relaxed);
        Ok(())
    }

    #[napi(getter)]
    pub fn get_onmessage(
        &self,
        env: Env,
    ) -> Result<Option<Function<'static, Unknown<'static>, Unknown<'static>>>> {
        handler_for(&env, &self.handlers, "message")
    }

    #[napi(setter)]
    pub fn set_onmessage(
        &self,
        _env: Env,
        handler: Option<Function<'static, Unknown<'static>, Unknown<'static>>>,
    ) -> Result<()> {
        set_handler(&self.handlers, "message", handler)
    }

    #[napi(getter)]
    pub fn get_onerror(
        &self,
        env: Env,
    ) -> Result<Option<Function<'static, Unknown<'static>, Unknown<'static>>>> {
        handler_for(&env, &self.handlers, "error")
    }

    #[napi(setter)]
    pub fn set_onerror(
        &self,
        _env: Env,
        handler: Option<Function<'static, Unknown<'static>, Unknown<'static>>>,
    ) -> Result<()> {
        set_handler(&self.handlers, "error", handler)
    }

    #[napi(getter)]
    pub fn get_onmessageerror(
        &self,
        env: Env,
    ) -> Result<Option<Function<'static, Unknown<'static>, Unknown<'static>>>> {
        handler_for(&env, &self.handlers, "messageerror")
    }

    #[napi(setter)]
    pub fn set_onmessageerror(
        &self,
        _env: Env,
        handler: Option<Function<'static, Unknown<'static>, Unknown<'static>>>,
    ) -> Result<()> {
        set_handler(&self.handlers, "messageerror", handler)
    }

    /// Register an event listener (mirrors `EventTarget.addEventListener`).
    #[napi]
    pub fn add_event_listener(
        &self,
        _env: Env,
        type_: String,
        listener: Function<'static, Unknown<'static>, Unknown<'static>>,
    ) -> Result<()> {
        let r = listener.create_ref()?;
        self.handlers
            .lock()
            .unwrap()
            .entry(type_)
            .or_default()
            .push(Arc::new(r));
        Ok(())
    }

    /// Remove a previously registered event listener.
    #[napi]
    pub fn remove_event_listener(
        &self,
        env: Env,
        type_: String,
        listener: Function<'static, Unknown<'static>, Unknown<'static>>,
    ) -> Result<()> {
        // Identity comparison needs the raw handles.
        let target = listener.raw();
        let mut map = self.handlers.lock().unwrap();
        let mut keep: Vec<SharedHandler> = Vec::new();
        for h in map.remove(&type_).unwrap_or_default() {
            let same = {
                let f: Function<Unknown<'static>, Unknown<'static>> = h.borrow_back(&env)?;
                f.raw() == target
            };
            if !same {
                keep.push(h);
            }
        }
        map.insert(type_, keep);
        Ok(())
    }

    /// Dispatch an event to this task's listeners (mirrors
    /// `EventTarget.dispatchEvent`).
    #[napi]
    pub fn dispatch_event(&self, env: Env, evt: Unknown<'static>) -> Result<()> {
        let evt_obj = evt.coerce_to_object()?;
        let type_: String = evt_obj.get_named_property("type")?;
        dispatch(&env, &self.handlers, &type_, &evt)
    }
}

/// Look up a single `on*` handler for `type_`.
fn handler_for(
    env: &Env,
    handlers: &HandlerMap,
    type_: &str,
) -> Result<Option<Function<'static, Unknown<'static>, Unknown<'static>>>> {
    let map = handlers.lock().unwrap();
    match map.get(type_).and_then(|v| v.first()) {
        Some(h) => {
            let f = h.borrow_back(env)?;
            let raw = f.raw();
            Ok(Some(unsafe { Function::from_napi_value(env.raw(), raw)? }))
        }
        None => Ok(None),
    }
}

/// Install or clear the `on*` handler for `type_`.
fn set_handler(
    handlers: &HandlerMap,
    type_: &str,
    handler: Option<Function<'static, Unknown<'static>, Unknown<'static>>>,
) -> Result<()> {
    let mut map = handlers.lock().unwrap();
    match handler {
        Some(f) => {
            let r = f.create_ref()?;
            map.insert(type_.to_string(), vec![Arc::new(r)]);
        }
        None => {
            map.remove(type_);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Running the resolved task (JS thread)
// ---------------------------------------------------------------------------

/// Evaluate the resolved task on the JS thread and emit `exit`.
fn run_resolved(
    env: &Env,
    handlers: &HandlerMap,
    terminated: &Arc<AtomicBool>,
    resolved: Option<std::result::Result<Vec<Programme>, String>>,
    task_data: Option<serde_json::Value>,
    terminate_slot: &TerminateSlot,
) -> Result<()> {
    // Emit `exit` and settle a pending `terminate()` promise with the code, so
    // `await task.terminate()` resolves when the task has actually stopped.
    let finish = |code: u32| -> Result<()> {
        emit_code(env, handlers, "exit", code)?;
        *terminate_slot.code.lock().unwrap() = Some(code);
        if let Some(d) = terminate_slot.pending.lock().unwrap().take() {
            d.resolve(Box::new(move |_env: Env| Ok(code)));
        }
        Ok(())
    };
    let resolved = resolved.unwrap_or(Ok(vec![Programme::Code(String::new())]));
    let entries = match resolved {
        Ok(entries) => entries,
        Err(reason) => {
            emit_data(env, handlers, "error", to_unknown(env, reason)?)?;
            finish(1)?;
            return Ok(());
        }
    };

    if terminated.load(Ordering::Relaxed) {
        finish(1)?;
        return Ok(());
    }

    // Run the entries in order: each shares the task's event handlers, so
    // `postMessage` / `onMessage` / `taskData` behave like one task script.
    // The first failure stops the sequence with exit code 1.
    for entry in &entries {
        let script = runner_script(entry);
        let runner: Function<
            (Unknown<'static>, Unknown<'static>, Unknown<'static>),
            Unknown<'static>,
        > = env.run_script(&script)?;

        // `postMessage` / `onMessage` are JS functions backed by Rust, so the
        // task gets a task-like messaging surface without a task.
        let out_handlers = Arc::clone(handlers);
        let post: Function<Unknown<'static>, ()> =
            env.create_function_from_closure("postMessage", move |ctx| {
                let value: Unknown<'static> = ctx.get(0)?;
                emit_data(&ctx.env, &out_handlers, "message", value)?;
                Ok(())
            })?;
        let in_handlers = Arc::clone(handlers);
        let on_msg: Function<Unknown<'static>, ()> =
            env.create_function_from_closure("onMessage", move |ctx| {
                let cb: Function<'static, Unknown<'static>, Unknown<'static>> = ctx.get(0)?;
                let r = cb.create_ref()?;
                in_handlers
                    .lock()
                    .unwrap()
                    .insert("__inbox".to_string(), vec![Arc::new(r)]);
                Ok(())
            })?;

        let data = to_unknown(env, task_data.clone().unwrap_or(serde_json::Value::Null))?;
        let argv = (to_unknown(env, post)?, to_unknown(env, on_msg)?, data);
        if let Err(e) = runner.call(argv) {
            emit_data(env, handlers, "error", to_unknown(env, e.reason)?)?;
            finish(1)?;
            return Ok(());
        }
    }
    finish(0)
}

/// Build the script that evaluates `task`.
///
/// The task is wrapped in a function taking `postMessage` / `onMessage` /
/// `taskData`, so it has the same messaging surface a task script would have
/// without needing a task.
fn runner_script(task: &Programme) -> String {
    match task {
        Programme::Code(code) => {
            format!("(function (postMessage, onMessage, taskData) {{\n{code}\n}})")
        }
        Programme::Module { path, source } => {
            let file = format!("{path:?}");
            let dir = Path::new(path)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let dir = format!("{dir:?}");
            // The module source is inlined rather than `require`d, so it shares
            // the wrapper's scope: `postMessage` / `onMessage` / `taskData` are
            // visible without publishing anything on `globalThis`. CommonJS
            // bindings (`require`, `module`, `exports`, `__filename`,
            // `__dirname`) are declared here to stand in for the module wrapper.
            format!(
                "(function (postMessage, onMessage, taskData) {{\n\
                 const {{ createRequire }} = process.mainModule.require('module');\n\
                 const __filename = {file};\n\
                 const __dirname = {dir};\n\
                 const require = createRequire(__filename);\n\
                 const module = {{ exports: {{}} }};\n\
                 const exports = module.exports;\n\
                 (function () {{\n{source}\n}})();\n\
                 return module.exports;\n\
                 }})"
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Task signals (libuv `uv_async_t`)
// ---------------------------------------------------------------------------

/// The task→event-loop wakeup primitive (libuv's `uv_async_t`), exposed to
/// JavaScript as the `TaskSignal` class.
///
/// Calling [`TaskSignal::send`] from *any* thread wakes the Node event loop and
/// schedules the `cb` given at construction to run on the loop thread. This is
/// exactly how a background [`Executor`] or the [`crate::Executor`] delivers
/// progress and results back to JavaScript: the task writes into shared state
/// and `send()`s, and the loop-side callback reads that state and invokes a
/// `ThreadSafeFunction`.
///
/// libuv **coalesces** `send()` calls (it is not a queue), so `cb` may run
/// fewer times than `send()` was called. Read fresh state inside `cb`; do not
/// rely on a 1:1 correspondence.
///
/// The type is `Copy` (a thin handle to a single `uv_async_t`); copies share
/// the same underlying handle and only one [`TaskSignal::close`] is required.
#[napi]
pub struct TaskSignal {
    inner: AsyncHandle,
}

impl Clone for TaskSignal {
    fn clone(&self) -> Self {
        *self
    }
}

impl Copy for TaskSignal {}

#[napi]
impl TaskSignal {
    /// Create a signal on the default (Node) libuv loop. `cb` runs on the loop
    /// thread every time [`TaskSignal::send`] is called (sends are coalesced).
    #[napi(constructor)]
    pub fn new(cb: Function<'static, (), ()>) -> Result<Self> {
        let tsfn = cb
            .build_threadsafe_function::<()>()
            .build()
            .map_err(|e| Error::new(Status::GenericFailure, format!("tsfn: {e}")))?;
        let loop_ = Loop::default()
            .map_err(|e| Error::new(Status::GenericFailure, format!("default loop: {e:?}")))?;
        Self::wake_on(&loop_, move || {
            let _ = tsfn.call((), ThreadsafeFunctionCallMode::NonBlocking);
        })
    }

    /// Wake the event loop. Safe to call from any thread; sends are coalesced,
    /// so the callback may run fewer times than `send` was called.
    #[napi]
    pub fn send(&self) -> Result<()> {
        let mut inner = self.inner;
        inner.send().map_err(|e| {
            Error::new(
                Status::GenericFailure,
                format!("uv_async_send failed: {e:?}"),
            )
        })
    }

    /// Close the signal, releasing the underlying `uv_async_t`. Call this once
    /// when the signal is no longer needed. `uv_close` cannot fail, so this
    /// always returns `Ok(())`.
    #[napi]
    pub fn close(&self) -> Result<()> {
        let mut inner = self.inner;
        inner.close(());
        Ok(())
    }
}

impl TaskSignal {
    /// Rust-side constructor used by the [`crate::Executor`]: create a signal on
    /// `loop_` whose `cb` (a plain Rust closure — e.g. one that reads shared
    /// task state and calls a `ThreadSafeFunction`) runs on the loop thread on
    /// every (coalesced) [`Task::send`].
    pub fn wake_on<F>(loop_: &Loop, mut cb: F) -> Result<Self>
    where
        F: FnMut() + 'static,
    {
        let inner = loop_
            .r#async(move |_handle: AsyncHandle| cb())
            .map_err(|e| {
                Error::new(
                    Status::GenericFailure,
                    format!("uv_async_init failed: {e:?}"),
                )
            })?;
        Ok(TaskSignal { inner })
    }
}

/// A pending `uv_task_t` request created by [`exec`]
/// ([`uv_queue_task`]).
///
/// `uv_queue_task` runs the task callback on a thread from the global libuv
/// thread pool and the after-task callback on the loop thread. This handle
/// represents the in-flight request and supports `uv_cancel`: cancellation
/// only succeeds if the task has not started yet (the after-task callback is
/// then invoked with status `UV_ECANCELED`).
#[napi]
pub struct TaskRequest {
    inner: TaskReq,
}

#[napi]
impl TaskRequest {
    /// `uv_cancel`: attempt to cancel the request. Returns `true` if the
    /// request was cancelled before it started, `false` if it had already
    /// started or completed.
    #[napi]
    pub fn cancel(&self) -> Result<bool> {
        let mut r: libuv::Req = libuv::Req::from(self.inner);
        r.cancel().map(|_| true).or_else(|_| Ok(false))
    }

    /// The libuv request type name (`"WORK"` for a `uv_task_t`).
    #[napi]
    pub fn type_name(&self) -> String {
        "WORK".to_string()
    }
}

/// `uv_queue_task(loop, req, task_cb, after_task_cb)` — schedule task on the
/// global libuv thread pool and report completion on the loop thread.
///
/// JavaScript cannot execute on a thread-pool thread, so both callbacks are
/// delivered through `ThreadsafeFunction`s: `task` is notified from the pool
/// thread when the request is picked up (it actually runs on the loop thread),
/// and `after` runs on the loop thread once the task completes. The returned
/// [`TaskRequest`] supports `uv_cancel` before the task starts.
#[napi]
pub fn run(
    task: Function<'static, (), ()>,
    after: Function<'static, (), ()>,
) -> Result<TaskRequest> {
    let task_tsfn = task
        .build_threadsafe_function::<()>()
        .build()
        .map_err(|e| Error::new(Status::GenericFailure, format!("tsfn: {e}")))?;
    let after_tsfn = after
        .build_threadsafe_function::<()>()
        .build()
        .map_err(|e| Error::new(Status::GenericFailure, format!("tsfn: {e}")))?;
    let pool = ThreadPool::new()?;
    pool.run(
        move |_req: TaskReq| {
            let _ = task_tsfn.call((), ThreadsafeFunctionCallMode::NonBlocking);
        },
        move |_req: TaskReq, _status| {
            let _ = after_tsfn.call((), ThreadsafeFunctionCallMode::NonBlocking);
        },
    )
    .map(|req| TaskRequest { inner: req })
}
