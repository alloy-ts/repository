//! A libuv-backed task with a Node-Worker-compatible surface.
//!
//! [`Task`] is a `uv_queue_work` request, not a Node worker thread. JavaScript
//! cannot execute on a libuv thread-pool thread, so the work is split:
//!
//!   * **libuv thread pool** — the blocking *setup*: creating the sandbox
//!     worktree, reading a `programme` blob out of the object database, fetching
//!     a remote `programme`.
//!   * **JS (V8) thread** — evaluating the resolved `programme` itself, in the
//!     after-work callback, via [`Env::run_script`].
//!
//! The Worker-shaped API (`threadId`, `postMessage`, `terminate`, `ref`/`unref`,
//! `onmessage`, `addEventListener`, ...) is kept for compatibility; it is driven
//! entirely from Rust.
//!
//! All state lives on the [`Task`] instance. There is no `globalThis` bootstrap
//! and no host-provided `Task` constructor: JS event handlers are held natively
//! through [`FunctionRef`], which keeps them alive for as long as the task does
//! and can be borrowed back on the JS thread to invoke them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use napi::bindgen_prelude::{
    ClassInstance, FromNapiValue, Function, FunctionRef, JsObjectValue, JsValue, Object,
    ToNapiValue, Unknown,
};
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{Env, Error, Result, Status};
use napi_derive::napi;

use libuv::WorkReq;

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
#[derive(Default)]
#[napi(object)]
pub struct TaskOptions {
    /// The task tree or runnable entry the task runs. It may be:
    ///
    ///   * a task name (defined in `tasks.json` or `package.json`),
    ///   * inline code text,
    ///   * a git object id naming a blob/tree in the object database,
    ///   * a filesystem path or `file://` URL to a script,
    ///   * a filesystem path or `file://` URL to a native `.node` addon, a
    ///     `.wasm` module, or another executable module format,
    ///   * an `http(s)` URL, which is fetched first (see `allowNetwork`).
    #[napi]
    pub task_tree: String,
    /// Worktree id (a directory name under `.git/worktrees/`) or a filesystem
    /// path to an existing worktree — either the main working tree or a linked
    /// worktree. Ignored when no worktree matches, falling back to `repository`
    /// and then the environment.
    ///
    /// Pass `"."` to run against the real current working directory. That
    /// explicitly opts out of the per-task temp worktree and takes precedence
    /// over `sandbox`.
    #[napi]
    pub worktree_path: Option<String>,
    /// Run the task isolated in its own worktree. When `true` and
    /// `worktreePath` is omitted, a worktree is created from the current HEAD at
    /// `${os.tmpdir()}/${taskId}`; when `worktreePath` is given, that location is
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
    /// Arbitrary data handed to the programme as `taskData`.
    #[napi]
    pub task_data: Option<serde_json::Value>,
    /// Forward unmanaged file descriptors into the task.
    #[napi]
    pub track_unmanaged_fds: Option<bool>,
    /// Permit fetching `taskTree` from a remote `http(s)` URL. Off by default:
    /// network access is never implicit. Local paths and `file://` URLs need no
    /// permission.
    #[napi]
    pub allow_network: Option<bool>,
    /// List of custom executable/runnable programme entries built into a task tree
    /// committed as an orphan commit and overlayed under `./tasks`.
    #[napi]
    pub task_entries: Option<Vec<TaskEntryOptions>>,
    /// Repository used to resolve `taskTree` when it is a git blob OID or task definition.
    /// When omitted, the repository is resolved from the environment (`GIT_DIR` / `REPO_DIR`,
    /// else discovery from the current directory).
    #[napi]
    pub repository: Option<ClassInstance<'static, Repository>>,
}

/// A custom task entry used to build a executable task tree.
#[derive(Clone, Default)]
#[napi(object)]
pub struct TaskEntryOptions {
    /// File path within the `./tasks` tree (e.g. `build.js` or `scripts/deploy.sh`).
    #[napi]
    pub path: String,
    /// File content (string or buffer code).
    #[napi]
    pub content: String,
    /// File mode (e.g., `"100755"` for executable or `"100644"` for default blob).
    #[napi]
    pub mode: Option<String>,
}

// ---------------------------------------------------------------------------
// Programme resolution
// ---------------------------------------------------------------------------

/// Module extensions handed to the runtime as a module path rather than as code.
const LOADABLE_EXTS: [&str; 5] = ["js", "cjs", "mjs", "node", "json"];

/// What `programme` resolved to.
enum Programme {
    /// Code text, evaluated on the JS thread.
    Code(String),
    /// A module: its path plus its source, read on the worker thread so the JS
    /// thread never blocks on file I/O. The source is inlined into the runner
    /// wrapper, so `postMessage` / `onMessage` / `taskData` are lexically in
    /// scope inside the module — no globals are published.
    Module { path: String, source: String },
}

/// Whether `spec` names the current working directory (the real filesystem the
/// process is running in), as opposed to a per-task temp worktree.
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

/// Download `url` into `dir` (the sandbox worktree when sandboxing, else a fresh
/// directory under the OS temp directory) and return the local path.
///
/// Shells out to `curl`, mirroring how the rest of this crate shells out to
/// `git`: no general-purpose HTTP client is in the dependency tree.
fn fetch_to_temp(url: &str, dir: Option<&Path>) -> Result<String> {
    let dir = match dir {
        Some(d) => d.to_path_buf(),
        None => std::env::temp_dir().join(format!("task-programme-{}", uuid::Uuid::new_v4())),
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
        .unwrap_or("programme");
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

/// Struct representing a dependency entry in task definitions.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(untagged)]
enum TaskDependency {
    Simple(String),
    Detailed {
        task: String,
        from: Option<String>,
    },
}

/// Struct representing a task configuration entry in `tasks.json` or `package.json`.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(untagged)]
enum TaskDef {
    Simple(String),
    Detailed {
        command: String,
        #[serde(default, rename = "dependsOn")]
        depends_on: Vec<TaskDependency>,
        cache: Option<serde_json::Value>,
    },
}

/// Helper to read and resolve task definitions from `tasks.json` or `package.json`.
fn load_task_command(dir: &Path, task_name: &str) -> Option<String> {
    let tasks_json = dir.join("tasks.json");
    let package_json = dir.join("package.json");

    let mut tasks_map: HashMap<String, TaskDef> = HashMap::new();

    if tasks_json.is_file() {
        if let Ok(content) = std::fs::read_to_string(&tasks_json) {
            if let Ok(parsed) = serde_json::from_str::<HashMap<String, TaskDef>>(&content) {
                tasks_map.extend(parsed);
            }
        }
    }

    if package_json.is_file() {
        if let Ok(content) = std::fs::read_to_string(&package_json) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(tasks_obj) = v.get("tasks") {
                    if let Ok(parsed) = serde_json::from_value::<HashMap<String, TaskDef>>(tasks_obj.clone()) {
                        tasks_map.extend(parsed);
                    }
                }
                if let Some(scripts_obj) = v.get("scripts") {
                    if let Ok(parsed) = serde_json::from_value::<HashMap<String, TaskDef>>(scripts_obj.clone()) {
                        for (k, v) in parsed {
                            tasks_map.entry(k).or_insert(v);
                        }
                    }
                }
            }
        }
    }

    if tasks_map.is_empty() {
        return None;
    }

    let mut visited = std::collections::HashSet::new();
    let mut execution_list = Vec::new();

    fn resolve_deps(
        name: &str,
        map: &HashMap<String, TaskDef>,
        visited: &mut std::collections::HashSet<String>,
        list: &mut Vec<String>,
    ) {
        if visited.contains(name) {
            return;
        }
        visited.insert(name.to_string());

        if let Some(def) = map.get(name) {
            if let TaskDef::Detailed { depends_on, .. } = def {
                for dep in depends_on {
                    let dep_task = match dep {
                        TaskDependency::Simple(s) => s.split('#').last().unwrap_or(s).to_string(),
                        TaskDependency::Detailed { task, .. } => task.clone(),
                    };
                    resolve_deps(&dep_task, map, visited, list);
                }
            }
            let cmd = match def {
                TaskDef::Simple(c) => c.clone(),
                TaskDef::Detailed { command, .. } => command.clone(),
            };
            list.push(cmd);
        }
    }

    if tasks_map.contains_key(task_name) {
        resolve_deps(task_name, &tasks_map, &mut visited, &mut execution_list);
        if !execution_list.is_empty() {
            return Some(execution_list.join("; "));
        }
    }

    None
}

/// Create an orphan commit containing a list of `task_entries` built into a separate tree,
/// and overlay/extract them to `./tasks` path under `target_dir`.
fn create_task_tree_orphan_commit_and_overlay(
    repo: &gix::Repository,
    entries: &[TaskEntryOptions],
    target_dir: &Path,
) -> Result<String> {
    let mut tree_entries = Vec::new();
    let tasks_dir = target_dir.join("tasks");
    std::fs::create_dir_all(&tasks_dir).map_err(|e| {
        Error::new(
            Status::GenericFailure,
            format!("failed to create tasks directory: {e}"),
        )
    })?;

    for entry in entries {
        let entry_path = Path::new(&entry.path);
        let dest = tasks_dir.join(entry_path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::new(
                    Status::GenericFailure,
                    format!("failed to create parent dir: {e}"),
                )
            })?;
        }
        std::fs::write(&dest, &entry.content).map_err(|e| {
            Error::new(
                Status::GenericFailure,
                format!("failed to write task file {}: {e}", dest.display()),
            )
        })?;

        let oid = repo
            .write_blob(entry.content.as_bytes())
            .map_err(|e| Error::new(Status::GenericFailure, format!("write_blob: {e}")))?
            .detach();

        let mode = match entry.mode.as_deref() {
            Some("100755") => gix::object::tree::EntryKind::BlobExecutable,
            _ => gix::object::tree::EntryKind::Blob,
        }
        .into();

        tree_entries.push(gix::objs::tree::Entry {
            mode,
            filename: entry.path.as_bytes().into(),
            oid,
        });
    }

    let tree = gix::objs::Tree {
        entries: tree_entries,
    };
    let tree_oid = repo
        .write_object(&tree)
        .map_err(|e| Error::new(Status::GenericFailure, format!("write_tree: {e}")))?
        .detach();

    let sig = gix::actor::Signature {
        name: "Task System".into(),
        email: "task@system.local".into(),
        time: gix::date::Time::now_local_or_utc(),
    };

    let commit = gix::objs::Commit {
        tree: tree_oid,
        parents: Vec::new().into(),
        author: sig.clone(),
        committer: sig,
        encoding: None,
        message: "Task tree executable entries".into(),
        extra_headers: Vec::new(),
    };

    let commit_oid = repo
        .write_object(&commit)
        .map_err(|e| Error::new(Status::GenericFailure, format!("write_commit: {e}")))?
        .detach();

    Ok(commit_oid.to_string())
}

/// If `programme` names a module file, return the local path to load. Handles
/// `file://` URLs, remote `http(s)` URLs (only when permitted) and plain
/// filesystem paths. Returns `None` when it is not a module reference.
fn module_file(
    programme: &str,
    sandbox_dir: Option<&Path>,
    allow_network: bool,
) -> Result<Option<String>> {
    if has_scheme(programme) {
        if let Some(rest) = programme.strip_prefix("file://") {
            return Ok(Some(rest.to_string()));
        }
        if programme.starts_with("http://") || programme.starts_with("https://") {
            if !allow_network {
                return Err(Error::new(
                    Status::GenericFailure,
                    format!(
                        "fetching `programme` over the network requires allowNetwork: true ({programme})"
                    ),
                ));
            }
            return fetch_to_temp(programme, sandbox_dir).map(Some);
        }
        return Ok(None);
    }
    let looks_like_module = Path::new(programme).is_file()
        || matches!(ext(programme), Some(e) if LOADABLE_EXTS.contains(&e.as_str()) || e == "wasm");
    if looks_like_module {
        Ok(Some(programme.to_string()))
    } else {
        Ok(None)
    }
}

/// Resolve `task_tree` into something the runtime can start, in order:
///
/// 1. a task name defined in `tasks.json` or `package.json` (`tasks` or `scripts`) → resolved command string,
/// 2. a git blob/tree OID present in the object database → its contents as code text,
/// 3. a `file://` / `http(s)` URL or filesystem path to a module → that local
///    path (`.wasm` is instantiated and `_start`ed from generated code),
/// 4. anything else → inline code text.
///
/// Runs on a libuv worker thread: it only touches the filesystem, the object
/// database and (optionally) the network, never V8.
fn resolve_programme(
    task_tree: &str,
    repo: Option<&Arc<gix::ThreadSafeRepository>>,
    sandbox_dir: Option<&Path>,
    worktree_path: Option<&Path>,
    allow_network: bool,
) -> Result<Programme> {
    let base_dir = sandbox_dir
        .map(PathBuf::from)
        .or_else(|| worktree_path.map(PathBuf::from))
        .or_else(|| std::env::current_dir().ok());

    let mut spec = task_tree;
    let mut resolved_command = None;

    if let Some(ref dir) = base_dir {
        if let Some(cmd) = load_task_command(dir, task_tree) {
            resolved_command = Some(cmd);
        }
    }

    if let Some(ref cmd) = resolved_command {
        spec = cmd.as_str();
    }

    if let (Ok(oid), Some(inner)) = (gix::ObjectId::from_hex(spec.as_bytes()), repo) {
        let r = thread_local(inner);
        let mut buf = Vec::new();
        if let Some(d) = Find::try_find(&r.objects, &oid, &mut buf)
            .map_err(|e| Error::new(Status::GenericFailure, format!("try_find {oid}: {e}")))?
        {
            if d.kind == gix::object::Kind::Blob {
                let code = String::from_utf8(d.data.to_vec()).map_err(|e| {
                    Error::new(
                        Status::GenericFailure,
                        format!("task_tree blob {oid} is not valid UTF-8: {e}"),
                    )
                })?;
                return Ok(Programme::Code(code));
            }
        }
    }
    if let Some(path) = module_file(spec, sandbox_dir, allow_network)? {
        if ext(&path) == Some("wasm".to_string()) {
            return Ok(Programme::Code(wasm_start(&path)));
        }
        let source = std::fs::read_to_string(&path).map_err(|e| {
            Error::new(
                Status::GenericFailure,
                format!("failed to read task_tree {path}: {e}"),
            )
        })?;
        return Ok(Programme::Module { path, source });
    }
    Ok(Programme::Code(spec.to_string()))
}

// ---------------------------------------------------------------------------
// Repository / worktree resolution
// ---------------------------------------------------------------------------

/// Create a linked worktree at `dir` from the current HEAD and open it as a
/// repository.
///
/// The worktree is created with a *detached* HEAD: a branch cannot be checked
/// out in two worktrees at once, so detaching is what lets any number of
/// sandboxed tasks run in parallel. Linked worktrees share the main
/// repository's object database, so creating one copies no objects.
fn create_sandboxed_worktree(dir: &Path) -> Result<Arc<gix::ThreadSafeRepository>> {
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
        .map_err(|e| err("add_worktree", e))?;
    // Reopen the administrative directory to obtain an owned, thread-safe handle.
    let git_dir = added.git_dir().to_path_buf();
    let ts = gix::ThreadSafeRepository::open(&git_dir).map_err(|e| err("worktree_open", e))?;
    Ok(Arc::new(ts))
}

/// Resolve `spec` — a worktree id (a directory name under `$GIT_DIR/worktrees/`)
/// or a filesystem path to a worktree checkout — to its repository. Returns
/// `None` when no worktree matches.
fn repo_from_worktree(
    spec: &str,
    repo: Option<&ClassInstance<'static, Repository>>,
) -> Option<Arc<gix::ThreadSafeRepository>> {
    let base = match repo {
        Some(r) => Arc::clone(&r.inner),
        None => crate::repository::current_repo().ok()?,
    };
    // 1. A registered worktree id: `$GIT_DIR/worktrees/<id>`.
    {
        let r = thread_local(&base);
        if let Ok(Some(proxy)) = r.worktree_proxy_by_id(spec) {
            if let Ok(ts) = gix::ThreadSafeRepository::open(proxy.git_dir()) {
                return Some(Arc::new(ts));
            }
        }
    }
    // 2. A filesystem path to a checkout — the main working tree or a linked one.
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
/// 1. a sandboxed worktree created for this task (`sandbox`),
/// 2. an existing worktree named by `worktreePath` (id or filesystem path),
/// 3. the explicitly supplied `repository`,
/// 4. the environment (`GIT_DIR` / `REPO_DIR`, else discovery from cwd).
///
/// Returns `None` when nothing can be resolved, in which case `programme` is run
/// as inline code text.
fn resolve_repo(
    options: &TaskOptions,
    sandbox_dir: Option<&Path>,
) -> Option<Arc<gix::ThreadSafeRepository>> {
    if let Some(dir) = sandbox_dir {
        if let Ok(repo) = create_sandboxed_worktree(dir) {
            return Some(repo);
        }
    }
    if let Some(spec) = options.worktree_path.as_deref() {
        if let Some(repo) = repo_from_worktree(spec, options.repository.as_ref()) {
            return Some(repo);
        }
    }
    if let Some(r) = options.repository.as_ref() {
        return Some(Arc::clone(&r.inner));
    }
    crate::repository::current_repo().ok()
}

/// Directory of the isolated worktree, when sandboxing. `.` explicitly selects
/// the real working directory, overriding the per-task temp worktree.
fn sandbox_dir(options: &TaskOptions, task_id: u32) -> Option<PathBuf> {
    match options.worktree_path.as_deref() {
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
/// moved into the libuv after-work callback.
type SharedHandler = Arc<FunctionRef<Unknown<'static>, Unknown<'static>>>;

/// Handlers registered for a task: the `on*` properties plus `addEventListener`
/// listeners, keyed by event type.
type HandlerMap = Arc<Mutex<HashMap<String, Vec<SharedHandler>>>>;

/// Buffered event to replay when a listener is registered after the event fired.
#[derive(Clone)]
enum BufferedEventData {
    Data(serde_json::Value),
    Code(u32),
}

#[derive(Clone)]
struct BufferedEvent {
    type_: String,
    data: BufferedEventData,
}

type BufferMap = Arc<Mutex<Vec<BufferedEvent>>>;

/// Invoke every handler registered for `type_` with `event`.
fn dispatch(env: &Env, handlers: &HandlerMap, type_: &str, event: &Unknown<'static>) -> Result<()> {
    for h in handlers.lock().unwrap().get(type_).into_iter().flatten() {
        let f: Function<Unknown<'static>, Unknown<'static>> = h.borrow_back(env)?;
        let _ = f.call(event.clone());
    }
    Ok(())
}

/// Emit a terminal event (`{ type, code }`).
fn emit_code(env: &Env, handlers: &HandlerMap, buffered: &BufferMap, type_: &str, code: u32) -> Result<()> {
    buffered.lock().unwrap().push(BufferedEvent {
        type_: type_.to_string(),
        data: BufferedEventData::Code(code),
    });
    let mut o = Object::new(env)?;
    o.set_named_property("type", type_)?;
    o.set_named_property("code", code)?;
    let evt = unsafe { Unknown::from_raw_unchecked(env.raw(), o.raw()) };
    dispatch(env, handlers, type_, &evt)
}

/// Emit a payload event (`{ type, data }`).
fn emit_data(env: &Env, handlers: &HandlerMap, buffered: &BufferMap, type_: &str, data: Unknown<'static>) -> Result<()> {
    let json_val: serde_json::Value = env.from_js_value(data.clone()).unwrap_or(serde_json::Value::Null);
    buffered.lock().unwrap().push(BufferedEvent {
        type_: type_.to_string(),
        data: BufferedEventData::Data(json_val),
    });
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

/// A libuv `uv_queue_work` task that presents a Node-Worker-compatible API.
#[napi]
pub struct Task {
    /// Process-unique id.
    id: u32,
    /// A stable, worker-like id for the task.
    thread_id: u32,
    /// The task's name (from construction options).
    name: String,
    /// Whether the task keeps the process alive (`true` by default).
    has_ref: Arc<AtomicBool>,
    /// The resource limits the task was created with.
    resource_limits: Option<TaskResourceLimits>,
    /// Whether [`Task::terminate`] has been called. Shared with the work
    /// callback so a terminated task stops before it evaluates its programme.
    terminated: Arc<AtomicBool>,
    /// JS event handlers and listeners, held natively.
    handlers: HandlerMap,
    /// Buffered events emitted before event listeners were attached.
    buffered_events: BufferMap,
    /// `taskData`, exposed to the programme.
    task_data: Option<serde_json::Value>,
    /// Keep threadsafe function alive on struct handle.
    _tsfn: Arc<dyn std::any::Any + Send + Sync>,
}

#[napi]
impl Task {
    /// Construct a new task that runs `options.task_tree`.
    #[napi(constructor)]
    pub fn new(env: Env, options: TaskOptions) -> Result<Self> {
        if (options.task_tree.starts_with("http://") || options.task_tree.starts_with("https://"))
            && !options.allow_network.unwrap_or(false)
        {
            return Err(Error::new(
                Status::GenericFailure,
                format!(
                    "fetching `taskTree` over the network requires allowNetwork: true ({})",
                    options.task_tree
                ),
            ));
        }

        let id = NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed);
        let thread_id = id;
        let name = options.name.clone().unwrap_or_default();
        let resource_limits = options.resource_limits.clone();
        let task_data = options.task_data.clone();

        let dir = sandbox_dir(&options, id);
        let allow_network = options.allow_network.unwrap_or(false);

        let handlers: HandlerMap = Arc::new(Mutex::new(HashMap::new()));
        let buffered_events: BufferMap = Arc::new(Mutex::new(Vec::new()));
        let terminated = Arc::new(AtomicBool::new(false));
        let has_ref = Arc::new(AtomicBool::new(true));

        let pool = ThreadPool::new()?;

        let result_slot: Arc<Mutex<Option<std::result::Result<Programme, String>>>> =
            Arc::new(Mutex::new(None));

        let tsfn_handlers = Arc::clone(&handlers);
        let tsfn_buffered = Arc::clone(&buffered_events);
        let tsfn_terminated = Arc::clone(&terminated);
        let tsfn_data = task_data.clone();
        let tsfn_result_slot = Arc::clone(&result_slot);
        let js_runner = env.create_function_from_closure("task_runner", move |ctx| {
            let resolved = tsfn_result_slot.lock().unwrap().take();
            run_resolved(
                &ctx.env,
                &tsfn_handlers,
                &tsfn_buffered,
                &tsfn_terminated,
                resolved,
                tsfn_data.clone(),
            )?;
            Ok(())
        })?;
        let tsfn = Arc::new(js_runner.build_threadsafe_function::<()>().build()?);

        // Work callback: a libuv thread-pool thread. No V8 access, so this is
        // limited to filesystem, object-database and network work.
        let work_terminated = Arc::clone(&terminated);
        let work_dir = dir.clone();
        let work_options_repo = options.repository.as_ref().map(|r| Arc::clone(&r.inner));
        let work_task_tree = options.task_tree.clone();
        let work_worktree = options.worktree_path.clone();
        let work_sandbox = options.sandbox;
        let work_task_entries = options.task_entries.clone();
        let work_result_slot = Arc::clone(&result_slot);
        let work_cb = move |_req: WorkReq| {
            // Resolving the sandbox worktree needs the same priority order as
            // `resolve_repo`, so reconstruct just enough of the options.
            let probe = TaskOptions {
                task_tree: String::new(),
                worktree_path: work_worktree.clone(),
                sandbox: work_sandbox,
                repository: None,
                ..Default::default()
            };
            let repo = if let Some(d) = work_dir.as_deref() {
                create_sandboxed_worktree(d)
                    .ok()
                    .or_else(|| work_options_repo.as_ref().map(Arc::clone))
            } else {
                resolve_repo(&probe, None).or_else(|| work_options_repo.as_ref().map(Arc::clone))
            };

            let target_dir_path = work_dir
                .as_deref()
                .or_else(|| work_worktree.as_deref().map(Path::new))
                .unwrap_or_else(|| Path::new("."));

            if let Some(entries) = &work_task_entries {
                if let Some(repo_ts) = &repo {
                    let r = thread_local(repo_ts);
                    let _ = create_task_tree_orphan_commit_and_overlay(&r, entries, target_dir_path);
                } else {
                    let tasks_dir = target_dir_path.join("tasks");
                    let _ = std::fs::create_dir_all(&tasks_dir);
                    for entry in entries {
                        let dest = tasks_dir.join(&entry.path);
                        if let Some(parent) = dest.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let _ = std::fs::write(&dest, &entry.content);
                    }
                }
            }

            let resolved = resolve_programme(
                &work_task_tree,
                repo.as_ref(),
                work_dir.as_deref(),
                work_worktree.as_deref().map(Path::new),
                allow_network,
            );
            let _ = work_terminated.load(Ordering::Relaxed);
            *work_result_slot.lock().unwrap() = Some(resolved.map_err(|e| e.reason));
        };

        let after_tsfn = Arc::clone(&tsfn);
        let after_cb = move |_req: WorkReq, _status| {
            let _ = after_tsfn.call((), ThreadsafeFunctionCallMode::NonBlocking);
        };

        pool.queue_task(work_cb, after_cb)
            .map_err(|e| Error::new(Status::GenericFailure, format!("queue_task: {e}")))?;

        Ok(Task {
            id,
            thread_id,
            name,
            has_ref,
            resource_limits,
            terminated,
            handlers,
            buffered_events,
            task_data,
            _tsfn: tsfn as Arc<dyn std::any::Any + Send + Sync>,
        })
    }

    /// The task's worker-like id. Mirrors `Worker.threadId`.
    #[napi(getter)]
    pub fn get_thread_id(&self) -> u32 {
        self.thread_id
    }

    /// Whether [`Task::terminate`] has been called.
    #[napi(getter)]
    pub fn get_terminated(&self) -> bool {
        self.terminated.load(Ordering::Relaxed)
    }

    /// Whether the task keeps the process alive (`true` by default). Mirrors
    /// `Worker.hasRef`.
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

    /// `taskData` handed to the programme.
    #[napi(getter)]
    pub fn get_task_data(&self, env: Env) -> Result<Unknown<'static>> {
        match &self.task_data {
            Some(v) => to_unknown(&env, v.clone()),
            None => to_unknown(&env, ()),
        }
    }

    /// Deliver `message` to the programme's `onMessage` handler, if it
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

    /// Stop the task. Resolves to the task's exit code.
    #[napi(ts_return_type = "Promise<number>")]
    pub fn terminate(&self, env: Env) -> Result<Unknown<'static>> {
        self.terminated.store(true, Ordering::Relaxed);
        // libuv cannot cancel work that has already started, so termination is
        // cooperative: the flag is observed before the programme is evaluated.
        to_unknown(&env, 1u32)
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
        env: Env,
        handler: Option<Function<'static, Unknown<'static>, Unknown<'static>>>,
    ) -> Result<()> {
        set_handler(&env, &self.handlers, &self.buffered_events, "message", handler)
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
        env: Env,
        handler: Option<Function<'static, Unknown<'static>, Unknown<'static>>>,
    ) -> Result<()> {
        set_handler(&env, &self.handlers, &self.buffered_events, "error", handler)
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
        env: Env,
        handler: Option<Function<'static, Unknown<'static>, Unknown<'static>>>,
    ) -> Result<()> {
        set_handler(&env, &self.handlers, &self.buffered_events, "messageerror", handler)
    }

    /// Register an event listener (mirrors `EventTarget.addEventListener`).
    #[napi]
    pub fn add_event_listener(
        &self,
        env: Env,
        type_: String,
        listener: Function<'static, Unknown<'static>, Unknown<'static>>,
    ) -> Result<()> {
        let r = listener.create_ref()?;
        self.handlers
            .lock()
            .unwrap()
            .entry(type_.clone())
            .or_default()
            .push(Arc::new(r));

        replay_buffered(&env, &self.buffered_events, &type_, &listener)?;
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
    env: &Env,
    handlers: &HandlerMap,
    buffered: &BufferMap,
    type_: &str,
    handler: Option<Function<'static, Unknown<'static>, Unknown<'static>>>,
) -> Result<()> {
    let mut map = handlers.lock().unwrap();
    match handler {
        Some(f) => {
            let r = f.create_ref()?;
            map.insert(type_.to_string(), vec![Arc::new(r)]);
            replay_buffered(env, buffered, type_, &f)?;
        }
        None => {
            map.remove(type_);
        }
    }
    Ok(())
}

fn replay_buffered(
    env: &Env,
    buffered: &BufferMap,
    type_: &str,
    listener: &Function<'static, Unknown<'static>, Unknown<'static>>,
) -> Result<()> {
    let list = buffered.lock().unwrap().clone();
    for item in list {
        if item.type_ == type_ {
            let mut o = Object::new(env)?;
            o.set_named_property("type", type_)?;
            match item.data {
                BufferedEventData::Data(v) => {
                    let val = to_unknown(env, v)?;
                    o.set_named_property("data", val.raw())?;
                }
                BufferedEventData::Code(c) => {
                    o.set_named_property("code", c)?;
                }
            }
            let evt = unsafe { Unknown::from_raw_unchecked(env.raw(), o.raw()) };
            let _ = listener.call(evt);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Running the resolved programme (JS thread)
// ---------------------------------------------------------------------------

/// Evaluate the resolved programme on the JS thread and emit `exit`.
fn run_resolved(
    env: &Env,
    handlers: &HandlerMap,
    buffered: &BufferMap,
    terminated: &Arc<AtomicBool>,
    resolved: Option<std::result::Result<Programme, String>>,
    task_data: Option<serde_json::Value>,
) -> Result<()> {
    let resolved = resolved.unwrap_or(Ok(Programme::Code(String::new())));
    let programme = match resolved {
        Ok(p) => p,
        Err(reason) => {
            emit_data(env, handlers, buffered, "error", to_unknown(env, reason)?)?;
            emit_code(env, handlers, buffered, "exit", 1)?;
            return Ok(());
        }
    };

    if terminated.load(Ordering::Relaxed) {
        emit_code(env, handlers, buffered, "exit", 1)?;
        return Ok(());
    }

    let script = runner_script(&programme);

    // `postMessage` / `onMessage` are JS functions backed by Rust, so the
    // programme gets a worker-like messaging surface without a worker.
    let out_handlers = Arc::clone(handlers);
    let out_buffered = Arc::clone(buffered);
    let post: Function<Unknown<'static>, ()> =
        env.create_function_from_closure("postMessage", move |ctx| {
            let value: Unknown<'static> = ctx.get(0)?;
            emit_data(&ctx.env, &out_handlers, &out_buffered, "message", value)?;
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

    let data = to_unknown(env, task_data.unwrap_or(serde_json::Value::Null))?;
    let argv = (to_unknown(env, post)?, to_unknown(env, on_msg)?, data);
    let runner: Function<
        (Unknown<'static>, Unknown<'static>, Unknown<'static>),
        Unknown<'static>,
    > = env.run_script(&script)?;

    match runner.call(argv) {
        Ok(_) => emit_code(env, handlers, buffered, "exit", 0)?,
        Err(e) => {
            emit_data(env, handlers, buffered, "error", to_unknown(env, e.reason)?)?;
            emit_code(env, handlers, buffered, "exit", 1)?;
        }
    }
    Ok(())
}

/// Build the script that evaluates `programme`.
///
/// The programme is wrapped in a function taking `postMessage` / `onMessage` /
/// `taskData`, so it has the same messaging surface a worker script would have
/// without needing a worker.
fn runner_script(programme: &Programme) -> String {
    match programme {
        Programme::Code(code) => format!("(function (postMessage, onMessage, taskData) {{\n{code}\n}})"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_task_command_simple() {
        let temp = tempfile::tempdir().unwrap();
        let tasks_file = temp.path().join("tasks.json");
        std::fs::write(
            &tasks_file,
            r#"{ "lint": "echo lint", "build": { "command": "echo build", "dependsOn": ["lint"] } }"#,
        )
        .unwrap();

        let cmd = load_task_command(temp.path(), "build");
        assert_eq!(cmd, Some("echo lint; echo build".to_string()));
    }

    #[test]
    fn test_load_task_command_detailed_deps() {
        let temp = tempfile::tempdir().unwrap();
        let pkg_file = temp.path().join("package.json");
        std::fs::write(
            &pkg_file,
            r#"{ "tasks": { "test": { "command": "vp test", "dependsOn": [{ "task": "build", "from": "dependencies" }] }, "build": "vp build" } }"#,
        )
        .unwrap();

        let cmd = load_task_command(temp.path(), "test");
        assert_eq!(cmd, Some("vp build; vp test".to_string()));
    }

    #[test]
    fn test_orphan_commit_overlay() {
        let temp = tempfile::tempdir().unwrap();
        let repo_dir = temp.path().join("repo");
        let repo = gix::init(&repo_dir).unwrap();

        let entries = vec![
            TaskEntryOptions {
                path: "run.js".to_string(),
                content: "console.log('task')".to_string(),
                mode: Some("100755".to_string()),
            },
        ];

        let commit_oid = create_task_tree_orphan_commit_and_overlay(&repo, &entries, &repo_dir).unwrap();
        assert!(!commit_oid.is_empty());
        assert!(repo_dir.join("tasks/run.js").is_file());
    }
}
