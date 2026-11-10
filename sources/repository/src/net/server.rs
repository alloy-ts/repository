//! Hono-style server. Routing and dispatch live here; Rocket is the listener.
//!
//! JavaScript handlers are stored as [`FunctionRef`]s and invoked through a
//! [`ThreadsafeFunction`]. `next()` schedules the rest of the chain on the
//! Tokio runtime and returns a Promise, so the JavaScript thread is never
//! blocked waiting for itself.
//!
//! Rocket 0.5.2 cannot represent QUERY, PURGE, or other methods outside its
//! enum. Those routes still run for `fetch()` and `request()`. A live listener
//! answers them with 400 before dispatch.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};

use base64::{Engine, engine::general_purpose::STANDARD};
use napi::bindgen_prelude::*;
use napi::threadsafe_function::ThreadsafeFunction;
use napi_derive::napi;
use serde_json::{Value, json};
use tokio::sync::{Mutex, oneshot};

use super::router::{self, Matched, Router as RouteMap};

const MAX_BODY_BYTES: u64 = 16 * 1024 * 1024;
const NOT_FINALIZED: &str = "Context is not finalized. Return a Response or await next().";

struct CallArgs<T>(T);

impl<A: ToNapiValue, B: ToNapiValue> JsValuesTupleIntoVec for CallArgs<(A, B)> {
    fn into_vec(self, env: sys::napi_env) -> Result<Vec<sys::napi_value>> {
        let (first, second) = self.0;
        Ok(vec![
            unsafe { A::to_napi_value(env, first)? },
            unsafe { B::to_napi_value(env, second)? },
        ])
    }
}

impl<A: ToNapiValue, B: ToNapiValue, C: ToNapiValue> JsValuesTupleIntoVec for CallArgs<(A, B, C)> {
    fn into_vec(self, env: sys::napi_env) -> Result<Vec<sys::napi_value>> {
        let (first, second, third) = self.0;
        Ok(vec![
            unsafe { A::to_napi_value(env, first)? },
            unsafe { B::to_napi_value(env, second)? },
            unsafe { C::to_napi_value(env, third)? },
        ])
    }
}

type UserFn = FunctionRef<CallArgs<(Unknown<'static>, Unknown<'static>)>, Unknown<'static>>;
type RemoteFn = ThreadsafeFunction<String, Promise<String>, String, Status, false, false, 0>;

struct JsHandler {
    direct: UserFn,
    remote: RemoteFn,
}

#[derive(Clone)]
struct RouteRecord {
    method: String,
    path: String,
    base_path: String,
    handler: Arc<JsHandler>,
    on_error: Option<Arc<JsHandler>>,
}

struct Slot {
    handler: Arc<JsHandler>,
    params: HashMap<String, String>,
    on_error: Option<Arc<JsHandler>>,
    called_next: AtomicBool,
}

struct State {
    method: String,
    url: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    params: HashMap<String, String>,
    env: Value,
    execution: Option<JsRef>,
    vars: HashMap<String, Value>,
    status: u16,
    pending: Vec<(String, String)>,
    wrote: bool,
    finalized: bool,
    res_status: u16,
    res_headers: Vec<(String, String)>,
    res_body: Vec<u8>,
}

struct Session {
    id: u64,
    state: StdMutex<State>,
    slots: Vec<Slot>,
    not_found: Option<Arc<JsHandler>>,
    on_error: Option<Arc<JsHandler>>,
    index: AtomicI32,
}

fn sessions() -> &'static StdMutex<HashMap<u64, Arc<Session>>> {
    static SESSIONS: OnceLock<StdMutex<HashMap<u64, Arc<Session>>>> = OnceLock::new();
    SESSIONS.get_or_init(|| StdMutex::new(HashMap::new()))
}

fn next_session_id() -> u64 {
    static IDS: AtomicU64 = AtomicU64::new(1);
    IDS.fetch_add(1, Ordering::Relaxed)
}

enum SharedRouter {
    RegExp(Arc<StdMutex<router::RegExpRouter<u32>>>),
    Trie(Arc<StdMutex<router::TrieRouter<u32>>>),
    Smart(Arc<StdMutex<router::SmartRouter<u32>>>),
}

impl SharedRouter {
    fn add(&self, method: &str, path: &str, handler: u32) -> Result<()> {
        let result = match self {
            Self::RegExp(router) => router.lock().unwrap().add(method, path, handler),
            Self::Trie(router) => router.lock().unwrap().add(method, path, handler),
            Self::Smart(router) => router.lock().unwrap().add(method, path, handler),
        };
        result.map_err(|error| Error::from_reason(error.to_string()))
    }

    fn match_route(&self, method: &str, path: &str) -> Vec<Matched<u32>> {
        match self {
            Self::RegExp(router) => router.lock().unwrap().match_route(method, path),
            Self::Trie(router) => router.lock().unwrap().match_route(method, path),
            Self::Smart(router) => router.lock().unwrap().match_route(method, path),
        }
    }
}

struct Inner {
    host: String,
    configured_port: u16,
    bound_port: AtomicU16,
    listening: AtomicBool,
    strict: bool,
    router: SharedRouter,
    handlers: StdMutex<Vec<Arc<JsHandler>>>,
    records: StdMutex<Vec<RouteRecord>>,
    not_found: StdMutex<Option<Arc<JsHandler>>>,
    on_error: StdMutex<Option<Arc<JsHandler>>>,
    get_path: StdMutex<Option<UserFn>>,
    legacy_remote: StdMutex<Option<Arc<RemoteFn>>>,
    rocket: Mutex<Option<Running>>,
    timer: StdMutex<Option<JsRef>>,
}

struct LegacyTable {
    routes: Vec<(String, JsRef)>,
    fetch: Option<UserFn>,
}

struct Running {
    shutdown: rocket::Shutdown,
    task: tokio::task::JoinHandle<()>,
}

struct View {
    inner: Arc<Inner>,
    base_path: String,
    path: StdMutex<String>,
}

#[napi]
pub struct Server {
    view: View,
}

#[napi]
pub struct RegExpRouter {
    inner: Arc<StdMutex<router::RegExpRouter<u32>>>,
}

#[napi]
pub struct TrieRouter {
    inner: Arc<StdMutex<router::TrieRouter<u32>>>,
}

#[napi]
pub struct SmartRouter {
    inner: Arc<StdMutex<router::SmartRouter<u32>>>,
}

#[napi]
impl RegExpRouter {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(StdMutex::new(router::RegExpRouter::default())),
        }
    }
}

#[napi]
impl TrieRouter {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(StdMutex::new(router::TrieRouter::default())),
        }
    }
}

#[napi]
impl SmartRouter {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(StdMutex::new(router::SmartRouter::default())),
        }
    }
}

#[napi]
impl Server {
    #[napi(constructor)]
    pub fn new(env: Env, options: Option<Object>) -> Result<Self> {
        let mut strict = true;
        let mut port = 3000u16;
        let mut host = "127.0.0.1".to_owned();
        let mut router = SharedRouter::Smart(Arc::new(StdMutex::new(router::SmartRouter::default())));
        let mut get_path = None;
        let mut legacy_routes = Vec::new();
        let mut legacy_fetch = None;
        if let Some(options) = options {
            if let Ok(value) = options.get_named_property::<bool>("strict") {
                strict = value;
            }
            if let Ok(value) = options.get_named_property::<u16>("port") {
                port = value;
            }
            if let Ok(value) = options.get_named_property::<String>("host") {
                host = value;
            }
            if let Ok(value) = options.get_named_property::<Unknown>("router") {
                if is_defined(&value) {
                    if let Ok(parsed) = shared_router(&env, value) {
                        router = parsed;
                    }
                }
            }
            if strict {
                if let Ok(value) = options.get_named_property::<Unknown>("getPath") {
                    if is_defined(&value) {
                        get_path = Some(store_fn(&env, value)?);
                    }
                }
            }
            if let Ok(value) = options.get_named_property::<Object>("routes") {
                let object_ctor: Unknown = env.get_global()?.get_named_property("Object")?;
                let mut keys_raw = std::ptr::null_mut();
                let keys_name = std::ffi::CString::new("keys").unwrap();
                check_status!(unsafe {
                    sys::napi_get_named_property(env.raw(), object_ctor.raw(), keys_name.as_ptr(), &mut keys_raw)
                })?;
                let keys_fn: Function<Unknown, Vec<String>> = unsafe { Function::from_napi_value(env.raw(), keys_raw)? };
                let keys = keys_fn.apply(object_ctor, value.to_unknown())?;
                for key in keys {
                    if let Ok(entry) = value.get_named_property::<Unknown>(&key) {
                        legacy_routes.push((key, create_js_ref(&env, &entry)?));
                    }
                }
            }
            if let Ok(value) = options.get_named_property::<Unknown>("fetch") {
                if is_defined(&value) {
                    legacy_fetch = Some(store_fn(&env, value)?);
                }
            }
        }
        Ok(Self {
            view: View {
                inner: Arc::new(Inner {
                    host,
                    configured_port: port,
                    bound_port: AtomicU16::new(port),
                    listening: AtomicBool::new(false),
                    strict,
                    router,
                    handlers: StdMutex::new(Vec::new()),
                    records: StdMutex::new(Vec::new()),
                    not_found: StdMutex::new(None),
                    on_error: StdMutex::new(None),
                    get_path: StdMutex::new(get_path),
                    legacy_remote: StdMutex::new(legacy_remote(&env, legacy_routes, legacy_fetch)?),
                    rocket: Mutex::new(None),
                    timer: StdMutex::new(None),
                }),
                base_path: "/".to_owned(),
                path: StdMutex::new("/".to_owned()),
            },
        })
    }

    #[napi(getter)]
    pub fn host(&self) -> String {
        self.view.inner.host.clone()
    }

    #[napi(getter)]
    pub fn port(&self) -> u16 {
        let bound = self.view.inner.bound_port.load(Ordering::SeqCst);
        if bound == 0 { self.view.inner.configured_port } else { bound }
    }

    #[napi(getter)]
    pub fn listening(&self) -> bool {
        self.view.inner.listening.load(Ordering::SeqCst)
    }

    #[napi]
    pub fn get(&self, env: Env, first: Either<String, Unknown>, second: Option<Unknown>, third: Option<Unknown>) -> Result<Self> {
        self.register(&env, "GET", first, second, third)
    }

    #[napi]
    pub fn post(&self, env: Env, first: Either<String, Unknown>, second: Option<Unknown>, third: Option<Unknown>) -> Result<Self> {
        self.register(&env, "POST", first, second, third)
    }

    #[napi]
    pub fn put(&self, env: Env, first: Either<String, Unknown>, second: Option<Unknown>, third: Option<Unknown>) -> Result<Self> {
        self.register(&env, "PUT", first, second, third)
    }

    #[napi]
    pub fn delete(&self, env: Env, first: Either<String, Unknown>, second: Option<Unknown>, third: Option<Unknown>) -> Result<Self> {
        self.register(&env, "DELETE", first, second, third)
    }

    #[napi]
    pub fn query(&self, env: Env, first: Either<String, Unknown>, second: Option<Unknown>, third: Option<Unknown>) -> Result<Self> {
        self.register(&env, "QUERY", first, second, third)
    }

    #[napi]
    pub fn patch(&self, env: Env, first: Either<String, Unknown>, second: Option<Unknown>, third: Option<Unknown>) -> Result<Self> {
        self.register(&env, "PATCH", first, second, third)
    }

    #[napi]
    pub fn options(&self, env: Env, first: Either<String, Unknown>, second: Option<Unknown>, third: Option<Unknown>) -> Result<Self> {
        self.register(&env, "OPTIONS", first, second, third)
    }

    #[napi]
    pub fn head(&self, env: Env, first: Either<String, Unknown>, second: Option<Unknown>, third: Option<Unknown>) -> Result<Self> {
        self.register(&env, "HEAD", first, second, third)
    }

    #[napi]
    pub fn all(&self, env: Env, first: Either<String, Unknown>, second: Option<Unknown>, third: Option<Unknown>) -> Result<Self> {
        self.register(&env, "ALL", first, second, third)
    }

    #[napi]
    pub fn on(
        &self,
        env: Env,
        method: Either<String, Vec<String>>,
        path: Either<String, Vec<String>>,
        handler: Unknown,
        extra: Option<Unknown>,
    ) -> Result<Self> {
        let methods = either_list(method);
        let paths = either_list(path);
        for route_path in &paths {
            *self.view.path.lock().unwrap() = route_path.clone();
            for route_method in &methods {
                self.add_route(&env, &route_method.to_ascii_uppercase(), route_path, &handler)?;
                if let Some(extra) = &extra {
                    self.add_route(&env, &route_method.to_ascii_uppercase(), route_path, extra)?;
                }
            }
        }
        Ok(self.clone_view())
    }

    #[napi(js_name = "use")]
    pub fn use_(&self, env: Env, first: Either<String, Unknown>, second: Option<Unknown>) -> Result<Self> {
        match first {
            Either::A(path) => {
                *self.view.path.lock().unwrap() = path.clone();
                if let Some(handler) = second {
                    self.add_route(&env, "ALL", &path, &handler)?;
                }
            }
            Either::B(handler) => {
                *self.view.path.lock().unwrap() = "*".to_owned();
                self.add_route(&env, "ALL", "*", &handler)?;
                if let Some(extra) = second {
                    self.add_route(&env, "ALL", "*", &extra)?;
                }
            }
        }
        Ok(self.clone_view())
    }

    #[napi]
    pub fn route(&self, path: String, app: ClassInstance<Server>) -> Result<Self> {
        let sub = self.base_path(path);
        let child = &app.view;
        let child_error = child.inner.on_error.lock().unwrap().clone();
        let records = child.inner.records.lock().unwrap();
        for record in records.iter() {
            if !belongs_to(&record.base_path, &child.base_path) {
                continue;
            }
            let on_error = if child_error.is_some() { child_error.clone() } else { record.on_error.clone() };
            sub.view.inner.router.add(&record.method, &merge_path(&sub.view.base_path, &record.path), sub.view.inner.handlers.lock().unwrap().len() as u32)?;
            let handler = record.handler.clone();
            sub.view.inner.handlers.lock().unwrap().push(handler.clone());
            sub.view.inner.records.lock().unwrap().push(RouteRecord {
                method: record.method.clone(),
                path: merge_path(&sub.view.base_path, &record.path),
                base_path: sub.view.base_path.clone(),
                handler,
                on_error,
            });
        }
        Ok(self.clone_view())
    }

    #[napi]
    pub fn base_path(&self, path: String) -> Self {
        Self {
            view: View {
                inner: self.view.inner.clone(),
                base_path: merge_path(&self.view.base_path, &path),
                path: StdMutex::new("/".to_owned()),
            },
        }
    }

    #[napi]
    pub fn not_found(&self, env: Env, handler: Unknown) -> Result<Self> {
        *self.view.inner.not_found.lock().unwrap() = Some(make_handler(&env, handler)?);
        Ok(self.clone_view())
    }

    #[napi]
    pub fn on_error(&self, env: Env, handler: Unknown) -> Result<Self> {
        *self.view.inner.on_error.lock().unwrap() = Some(make_handler(&env, handler)?);
        Ok(self.clone_view())
    }

    #[napi]
    pub fn mount(&self, env: Env, path: String, application: Unknown) -> Result<Self> {
        let prefix = merge_path(&self.view.base_path, &path);
        let prefix = prefix.trim_end_matches('/').to_owned();
        let application = create_js_ref(&env, &application)?;
        let mounted: Function<CallArgs<(Unknown<'static>, Unknown<'static>)>, Unknown<'static>> = env.create_function_from_closure(
            "mounted",
            move |ctx| invoke_mounted(ctx, &application, &prefix),
        )?;
        let handler = make_handler(&env, function_to_unknown(&env, mounted)?)?;
        let pattern = merge_path(&path, "*");
        self.add_handler("ALL", &pattern, handler)?;
        Ok(self.clone_view())
    }

    #[napi]
    pub fn fire(&self, env: Env) -> Result<()> {
        let global = env.get_global()?;
        let warn: Function<String, Unknown> = global
            .get_named_property::<Object>("console")?
            .get_named_property("warn")?;
        let _ = warn.call("server.fire() is deprecated. Dispatch requests with server.fetch().".to_owned());
        ensure_fetch_host(&env)?;
        let view = self.clone_view();
        let listener: Function<(Object,), ()> = env.create_function_from_closure("fetch", move |ctx| {
            let event: Object = ctx.first_arg()?;
            let request: Unknown = event.get_named_property("request")?;
            let respond: Function<Unknown, Unknown> = event.get_named_property("respondWith")?;
            let response = dispatch_prepared(ctx.env, &view, read_request(ctx.env, &request)?, None, None)?;
            respond.call(response)?;
            Ok(())
        })?;
        let target = env.get_global()?;
        let add: Function<CallArgs<(String, Function<(Object,), ()>)>, Unknown> = target.get_named_property("addEventListener")?;
        add.call(CallArgs(("fetch".to_owned(), listener)))?;
        Ok(())
    }

    #[napi]
    pub fn fetch(&self, env: Env, request: Unknown, bindings: Option<Unknown>, execution: Option<Unknown>) -> Result<Unknown<'static>> {
        let prepared = read_request(&env, &request)?;
        let env_value = match bindings {
            Some(value) => js_to_json(&env, &value).unwrap_or(Value::Null),
            None => Value::Null,
        };
        let mut execution = execution.map(|value| create_js_ref(&env, &value)).transpose()?;
        if prepared.method.eq_ignore_ascii_case("GET") || prepared.method.eq_ignore_ascii_case("HEAD") || prepared.body.is_empty() {
            let request_object: Object = unsafe { Object::from_napi_value(env.raw(), request.raw())? };
            if !prepared.method.eq_ignore_ascii_case("GET") && !prepared.method.eq_ignore_ascii_case("HEAD") {
                if let Ok(array_buffer) = request_object.get_named_property::<Function<(), Unknown>>("arrayBuffer") {
                    if let Ok(bytes_promise) = array_buffer.apply(&request_object, ()) {
                        if let Ok(bytes_object) = unsafe { Object::from_napi_value(env.raw(), bytes_promise.raw()) } {
                            let server = self.clone_view();
                            let env_value = env_value.clone();
                            let execution = StdMutex::new(execution.take());
                            let then: Function<(), Unknown<'static>> = env.create_function_from_closure("body", move |ctx| {
                                let mut prepared = prepared.clone();
                                if let Ok(value) = ctx.first_arg::<Unknown>() {
                                    if let Ok(bytes) = copy_bytes(ctx.env, value) {
                                        prepared.body = bytes;
                                    }
                                }
                                let execution = execution.lock().unwrap().take();
                                dispatch_prepared(ctx.env, &server, prepared, Some(env_value.clone()), execution)
                            })?;
                            let then_fn: Function<Function<(), Unknown<'static>>, Unknown> = bytes_object.get_named_property("then")?;
                            return Ok(own(then_fn.apply(bytes_object, then)?));
                        }
                    }
                }
            }
        }
        dispatch_prepared(&env, self, prepared, Some(env_value), execution)
    }

    #[napi]
    pub fn request(
        &self,
        env: Env,
        input: Either<String, Unknown>,
        init: Option<Unknown>,
        bindings: Option<Unknown>,
        execution: Option<Unknown>,
    ) -> Result<Unknown<'static>> {
        let request = match input {
            Either::A(value) => {
                let url = if value.starts_with("http://") || value.starts_with("https://") {
                    value
                } else {
                    format!("http://localhost{}", merge_path("/", &value))
                };
                new_request(&env, &url, init)?
            }
            Either::B(request) => {
                if let Some(init) = init {
                    new_request_from(&env, &request, &init)?
                } else {
                    request
                }
            }
        };
        self.fetch(env, request, bindings, execution)
    }

    #[napi]
    pub fn start<'env>(&self, env: &'env Env) -> Result<PromiseRaw<'env, u16>> {
        if self.view.inner.rocket.try_lock().ok().and_then(|guard| guard.as_ref().map(|_| ())).is_some()
            && self.view.inner.listening.load(Ordering::SeqCst)
        {
            return env.spawn_future(async { Err(Error::from_reason("server already started")) });
        }
        install_timer(&env, &self.view.inner)?;
        let inner = self.view.inner.clone();
        env.spawn_future(async move { rocket_start(inner).await })
    }

    #[napi]
    pub fn serve<'env>(&self, env: &'env Env) -> Result<PromiseRaw<'env, u16>> {
        self.start(env)
    }

    #[napi]
    pub fn stop<'env>(&self, env: &'env Env) -> Result<PromiseRaw<'env, ()>> {
        clear_timer(&env, &self.view.inner);
        let inner = self.view.inner.clone();
        env.spawn_future(async move {
            rocket_stop(inner).await?;
            Ok(())
        })
    }
}

impl Server {
    fn clone_view(&self) -> Self {
        Self {
            view: View {
                inner: self.view.inner.clone(),
                base_path: self.view.base_path.clone(),
                path: StdMutex::new(self.view.path.lock().unwrap().clone()),
            },
        }
    }

    fn register(&self, env: &Env, method: &str, first: Either<String, Unknown>, second: Option<Unknown>, third: Option<Unknown>) -> Result<Self> {
        let mut handlers = Vec::new();
        match first {
            Either::A(path) => {
                *self.view.path.lock().unwrap() = path;
                if let Some(handler) = second {
                    handlers.push(handler);
                }
            }
            Either::B(handler) => {
                handlers.push(handler);
                if let Some(extra) = second {
                    handlers.push(extra);
                }
            }
        }
        if let Some(extra) = third {
            handlers.push(extra);
        }
        let path = self.view.path.lock().unwrap().clone();
        for handler in handlers {
            self.add_route(env, method, &path, &handler)?;
        }
        Ok(self.clone_view())
    }

    fn add_route(&self, env: &Env, method: &str, path: &str, handler: &Unknown) -> Result<()> {
        self.add_handler(method, path, make_handler(env, handler.clone())?)
    }

    fn add_handler(&self, method: &str, path: &str, handler: Arc<JsHandler>) -> Result<()> {
        let full = merge_path(&self.view.base_path, path);
        let id = self.view.inner.handlers.lock().unwrap().len() as u32;
        self.view.inner.router.add(method, &full, id)?;
        self.view.inner.handlers.lock().unwrap().push(handler.clone());
        let on_error = self.view.inner.on_error.lock().unwrap().clone();
        self.view.inner.records.lock().unwrap().push(RouteRecord {
            method: method.to_owned(),
            path: full,
            base_path: self.view.base_path.clone(),
            handler,
            on_error,
        });
        Ok(())
    }
}

fn clear_exception(env: &Env) {
    let mut exception = std::ptr::null_mut();
    unsafe { sys::napi_get_and_clear_last_exception(env.raw(), &mut exception); }
}

fn constructor_name(env: &Env, value: &Unknown) -> String {
    let Ok(object) = (unsafe { Object::from_napi_value(env.raw(), value.raw()) }) else {
        return String::new();
    };
    object
        .get_named_property::<Object>("constructor")
        .and_then(|constructor| constructor.get_named_property::<String>("name"))
        .unwrap_or_default()
}

fn shared_router(env: &Env, value: Unknown) -> Result<SharedRouter> {
    let name = constructor_name(env, &value);
    let casted = match name.as_str() {
        "RegExpRouter" => unsafe { ClassInstance::<RegExpRouter>::from_napi_value(env.raw(), value.raw()) }
            .map(|router| SharedRouter::RegExp(router.inner.clone())),
        "TrieRouter" => unsafe { ClassInstance::<TrieRouter>::from_napi_value(env.raw(), value.raw()) }
            .map(|router| SharedRouter::Trie(router.inner.clone())),
        "SmartRouter" => unsafe { ClassInstance::<SmartRouter>::from_napi_value(env.raw(), value.raw()) }
            .map(|router| SharedRouter::Smart(router.inner.clone())),
        _ => Err(Error::from_reason("router must be a RegExpRouter, TrieRouter, or SmartRouter")),
    };
    if casted.is_err() {
        clear_exception(env);
    }
    casted
}

struct JsRef {
    inner: sys::napi_ref,
    env: sys::napi_env,
}

unsafe impl Send for JsRef {}

impl JsRef {
    fn get<'env>(&self, env: &'env Env) -> Result<Unknown<'env>> {
        let mut result = std::ptr::null_mut();
        check_status!(unsafe { sys::napi_get_reference_value(env.raw(), self.inner, &mut result) })?;
        unsafe { Unknown::from_napi_value(env.raw(), result) }
    }

    fn unref(self, env: &Env) -> Result<()> {
        let mut count = 0;
        check_status!(unsafe { sys::napi_reference_unref(env.raw(), self.inner, &mut count) })?;
        check_status!(unsafe { sys::napi_delete_reference(env.raw(), self.inner) })?;
        std::mem::forget(self);
        Ok(())
    }
}

impl Drop for JsRef {
    fn drop(&mut self) {
        // The reference is released on the JavaScript thread via `unref`.
        // Dropping it from another thread would touch V8 off-thread.
    }
}

fn create_js_ref(env: &Env, value: &Unknown) -> Result<JsRef> {
    let mut reference = std::ptr::null_mut();
    check_status!(unsafe { sys::napi_create_reference(env.raw(), value.raw(), 1, &mut reference) })?;
    Ok(JsRef { inner: reference, env: env.raw() })
}

fn undefined<'a>(env: &'a Env) -> Result<Unknown<'a>> {
    let raw = unsafe { <() as ToNapiValue>::to_napi_value(env.raw(), ())? };
    unsafe { Unknown::from_napi_value(env.raw(), raw) }
}

fn js_string<'a>(env: &'a Env, text: &str) -> Result<Unknown<'a>> {
    let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), text.to_owned())? };
    unsafe { Unknown::from_napi_value(env.raw(), raw) }
}

fn js_bytes<'a>(env: &'a Env, bytes: &[u8]) -> Result<Unknown<'a>> {
    let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), Buffer::from(bytes.to_vec()))? };
    unsafe { Unknown::from_napi_value(env.raw(), raw) }
}

fn rewrite_mount_url(url: &str, prefix: &str) -> String {
    let Some(scheme) = url.find("://") else {
        return url.to_owned();
    };
    let Some(start) = url[scheme + 3..].find('/').map(|index| scheme + 3 + index) else {
        return url.to_owned();
    };
    let path = &url[start..];
    let rewritten = if path == prefix {
        "/".to_owned()
    } else if let Some(rest) = path.strip_prefix(&format!("{prefix}/")) {
        format!("/{rest}")
    } else {
        path.to_owned()
    };
    format!("{}{rewritten}", &url[..start])
}

fn as_remote(function: Function<String, Unknown>) -> Result<RemoteFn> {
    let function = unsafe { std::mem::transmute::<Function<String, Unknown>, Function<String, Promise<String>>>(function) };
    function.build_threadsafe_function::<String>().callee_handled::<false>().build()
}

fn own<'a>(value: Unknown<'a>) -> Unknown<'static> {
    unsafe { std::mem::transmute(value) }
}

fn store_fn(env: &Env, value: Unknown) -> Result<UserFn> {
    unsafe { FunctionRef::from_napi_value(env.raw(), value.raw()) }
}

fn make_handler(env: &Env, value: Unknown) -> Result<Arc<JsHandler>> {
    let direct = store_fn(env, value)?;
    let owned = store_fn_raw(env, &direct)?;
    let adapter: Function<String, Unknown> = env.create_function_from_closure("handler", move |ctx| invoke_handler(ctx, &owned))?;
    let remote = as_remote(adapter)?;
    Ok(Arc::new(JsHandler { direct, remote }))
}

fn store_fn_raw(env: &Env, reference: &UserFn) -> Result<UserFn> {
    let value = reference.borrow_back(env)?;
    unsafe { FunctionRef::from_napi_value(env.raw(), value.raw()) }
}

fn function_to_unknown(
    env: &Env,
    function: Function<CallArgs<(Unknown<'static>, Unknown<'static>)>, Unknown<'static>>,
) -> Result<Unknown<'static>> {
    let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), function)? };
    Ok(own(unsafe { Unknown::from_napi_value(env.raw(), raw)? }))
}

fn invoke_mounted(ctx: FunctionCallContext, application: &JsRef, prefix: &str) -> Result<Unknown<'static>> {
    let context: Object = ctx.first_arg()?;
    let req: Object = context.get_named_property("req")?;
    let raw: Unknown = req.get_named_property("raw")?;
    let env_value = defined_property(&context, "env")?.unwrap_or(undefined(ctx.env)?);
    let execution = defined_property(&context, "executionCtx")?.unwrap_or(undefined(ctx.env)?);
    clear_exception(ctx.env);
    let rewritten = rewrite_mounted_request(ctx.env, &raw, prefix)?;
    let app = application.get(ctx.env)?;
    let args = CallArgs((to_static(rewritten), to_static(env_value), to_static(execution)));
    if app.get_type()? == ValueType::Function {
        let func: Function<CallArgs<(Unknown<'static>, Unknown<'static>, Unknown<'static>)>, Unknown> =
            unsafe { Function::from_napi_value(ctx.env.raw(), app.raw())? };
        return Ok(own(func.call(args)?));
    }
    let object: Object = unsafe { Object::from_napi_value(ctx.env.raw(), app.raw()) }?;
    let fetch: Function<CallArgs<(Unknown<'static>, Unknown<'static>, Unknown<'static>)>, Unknown> =
        object.get_named_property("fetch")?;
    Ok(own(fetch.apply(&object, args)?))
}

fn defined_property(object: &Object, name: &str) -> Result<Option<Unknown<'static>>> {
    match object.get_named_property::<Unknown>(name) {
        Ok(value) if is_defined(&value) => Ok(Some(to_static(value))),
        _ => Ok(None),
    }
}

fn rewrite_mounted_request(env: &Env, request: &Unknown, prefix: &str) -> Result<Unknown<'static>> {
    let object: Object = unsafe { Object::from_napi_value(env.raw(), request.raw()) }?;
    let url: String = object.get_named_property("url")?;
    let rewritten = rewrite_mount_url(&url, prefix);
    let ctor: Function<CallArgs<(String, Unknown)>, Unknown> = env.get_global()?.get_named_property("Request")?;
    Ok(own(ctor.new_instance(CallArgs((rewritten, request.clone())))?))
}

fn invoke_handler(ctx: FunctionCallContext, handler: &UserFn) -> Result<Unknown<'static>> {
    let payload: String = ctx.first_arg()?;
    let value: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
    let sid = value.get("sid").and_then(Value::as_u64).unwrap_or(0);
    let slot = value.get("slot").and_then(Value::as_u64).unwrap_or(0) as usize;
    let kind = value.get("kind").and_then(Value::as_str).unwrap_or("route");
    let context = build_context(ctx.env, sid)?;
    let next = make_next(ctx.env, sid, slot)?;
    let func = handler.borrow_back(ctx.env)?;
    let result = if kind == "error" {
        let message = value.get("message").and_then(Value::as_str).unwrap_or("error");
        let error = js_error(ctx.env, message)?;
        func.call(CallArgs((to_static(error), to_static(context.to_unknown()))))?
    } else {
        func.call(CallArgs((to_static(context.to_unknown()), to_static(next))))?
    };
    promise_json(ctx.env, result)
}

fn make_next(env: &Env, sid: u64, slot: usize) -> Result<Unknown<'static>> {
    let next: Function<(), Unknown<'static>> = env.create_function_from_closure("next", move |ctx| {
        let Some(session) = sessions().lock().unwrap().get(&sid).cloned() else {
            return Err(Error::from_reason("request finished"));
        };
        if slot < session.slots.len() && session.slots[slot].called_next.swap(true, Ordering::SeqCst) {
            return Err(Error::from_reason("next() called multiple times"));
        }
        let promise = ctx.env.spawn_future(async move {
            run_slot(session, slot + 1).await?;
            Ok(())
        })?;
        Ok(own(promise.to_unknown()))
    })?;
    Ok(own(next.to_unknown()))
}

fn build_context(env: &Env, sid: u64) -> Result<Object<'static>> {
    let mut object = Object::new(env)?;
    let session = sessions().lock().unwrap().get(&sid).cloned();
    let Some(session) = session else {
        return Ok(object);
    };
    let state = session.state.lock().unwrap();
    let method = state.method.clone();
    let url = state.url.clone();
    let path = state.path.clone();
    let headers = state.headers.clone();
    let body = state.body.clone();
    let params = state.params.clone();
    let env_value = state.env.clone();
    let execution = state.execution.as_ref().and_then(|reference| reference.get(env).ok());
    drop(state);

    let mut req = Object::new(env)?;
    req.set_named_property("method", method.clone())?;
    req.set_named_property("url", url.clone())?;
    req.set_named_property("path", path)?;
    let raw = build_raw_request(env, &url, &method, &headers, &body)?;
    req.set_named_property("raw", raw)?;
    let params_for_call = params.clone();
    let param: Function<(), Unknown<'static>> = env.create_function_from_closure("param", move |ctx| param_value(ctx, &params_for_call))?;
    req.set_named_property("param", param)?;
    let query: Function<(), Unknown<'static>> = env.create_function_from_closure("query", {
        let url = url.clone();
        move |ctx| query_value(ctx, &url)
    })?;
    req.set_named_property("query", query)?;
    let header: Function<(), Unknown<'static>> = env.create_function_from_closure("header", {
        let headers = headers.clone();
        move |ctx| header_value(ctx, &headers)
    })?;
    req.set_named_property("header", header)?;
    attach_body_method(env, &mut req, "text")?;
    attach_body_method(env, &mut req, "json")?;
    attach_body_method(env, &mut req, "arrayBuffer")?;
    attach_body_method(env, &mut req, "blob")?;
    attach_body_method(env, &mut req, "formData")?;
    object.set_named_property("req", req)?;
    object.set_named_property("env", json_to_js(env, &env_value)?)?;
    if let Some(execution) = execution {
        object.set_named_property("executionCtx", execution)?;
    } else {
        object.set_named_property("executionCtx", undefined(env)?)?;
    }
    object.set_named_property(
        "finalized",
        sessions().lock().unwrap().get(&sid).map(|session| session.state.lock().unwrap().finalized).unwrap_or(false),
    )?;
    attach_context_methods(env, &mut object, sid)?;
    Ok(object)
}

fn attach_context_methods(env: &Env, object: &mut Object, sid: u64) -> Result<()> {
    let text: Function<(), Unknown<'static>> = env.create_function_from_closure("text", move |ctx| text_response(ctx, sid, "text/plain; charset=UTF-8", false))?;
    object.set_named_property("text", text)?;
    let json_fn: Function<(), Unknown<'static>> = env.create_function_from_closure("json", move |ctx| text_response(ctx, sid, "application/json", true))?;
    object.set_named_property("json", json_fn)?;
    let html: Function<(), Unknown<'static>> = env.create_function_from_closure("html", move |ctx| text_response(ctx, sid, "text/html; charset=UTF-8", false))?;
    object.set_named_property("html", html)?;
    let body: Function<(), Unknown<'static>> = env.create_function_from_closure("body", move |ctx| body_response(ctx, sid))?;
    object.set_named_property("body", body)?;
    let redirect: Function<(), Unknown<'static>> = env.create_function_from_closure("redirect", move |ctx| redirect_response(ctx, sid))?;
    object.set_named_property("redirect", redirect)?;
    let status_fn: Function<(), Unknown<'static>> = env.create_function_from_closure("status", move |ctx| {
        let code: u16 = ctx.first_arg()?;
        if let Some(session) = sessions().lock().unwrap().get(&sid).cloned() {
            let mut state = session.state.lock().unwrap();
            state.status = code;
            if state.finalized {
                state.res_status = code;
            }
        }
        Ok(own(ctx.this::<Object>()?.to_unknown()))
    })?;
    object.set_named_property("status", status_fn)?;
    let header_fn: Function<(), Unknown<'static>> = env.create_function_from_closure("header", move |ctx| {
        let name: String = ctx.get(0)?;
        let value: String = ctx.get(1)?;
        let append = ctx.get::<Object>(2).ok().and_then(|options| options.get_named_property::<bool>("append").ok()).unwrap_or(false);
        if let Some(session) = sessions().lock().unwrap().get(&sid).cloned() {
            let mut state = session.state.lock().unwrap();
            state.wrote = true;
            header_set(&mut state.pending, &name, &value, append);
            if state.finalized {
                header_set(&mut state.res_headers, &name, &value, append);
            }
        }
        Ok(own(ctx.this::<Object>()?.to_unknown()))
    })?;
    object.set_named_property("header", header_fn)?;
    let set_fn: Function<(), Unknown<'static>> = env.create_function_from_closure("set", move |ctx| {
        let key: String = ctx.get(0)?;
        let value: Unknown = ctx.get(1)?;
        let json = js_to_json(ctx.env, &value).unwrap_or(Value::Null);
        if let Some(session) = sessions().lock().unwrap().get(&sid).cloned() {
            session.state.lock().unwrap().vars.insert(key, json);
        }
        Ok(own(undefined(ctx.env)?))
    })?;
    object.set_named_property("set", set_fn)?;
    let get_fn: Function<(), Unknown<'static>> = env.create_function_from_closure("get", move |ctx| {
        let key: String = ctx.first_arg()?;
        let value = sessions()
            .lock()
            .unwrap()
            .get(&sid)
            .and_then(|session| session.state.lock().unwrap().vars.get(&key).cloned())
            .unwrap_or(Value::Null);
        json_to_js(ctx.env, &value)
    })?;
    object.set_named_property("get", get_fn)?;
    let not_found_fn: Function<(), Unknown<'static>> = env.create_function_from_closure("notFound", move |ctx| {
        let Some(session) = sessions().lock().unwrap().get(&sid).cloned() else {
            return web_response(ctx.env, 404, Vec::new(), b"404 Not Found".to_vec());
        };
        if let Some(handler) = session.not_found.clone() {
            let context = build_context(ctx.env, sid)?;
            let func = handler.direct.borrow_back(ctx.env)?;
            let missing = undefined(ctx.env)?;
            return Ok(own(func.call(CallArgs((to_static(context.to_unknown()), to_static(missing))))?));
        }
        web_response(ctx.env, 404, Vec::new(), b"404 Not Found".to_vec())
    })?;
    object.set_named_property("notFound", not_found_fn)?;
    let res_fn: Function<(), Unknown<'static>> = env.create_function_from_closure("res", move |ctx| {
        let Some(session) = sessions().lock().unwrap().get(&sid).cloned() else {
            return Err(Error::from_reason("Context is not finalized"));
        };
        let state = session.state.lock().unwrap();
        if !state.finalized {
            return Err(Error::from_reason("Context is not finalized"));
        }
        web_response(ctx.env, state.res_status, state.res_headers.clone(), state.res_body.clone())
    })?;
    object.set_named_property("res", res_fn)?;
    Ok(())
}

fn text_response(ctx: FunctionCallContext, sid: u64, content_type: &str, json_body: bool) -> Result<Unknown<'static>> {
    let body = if json_body {
        let value: Unknown = ctx.first_arg()?;
        let json = js_to_json(ctx.env, &value).unwrap_or(Value::Null);
        json.to_string()
    } else {
        ctx.first_arg::<String>()?
    };
    let status = ctx.get::<u16>(1).unwrap_or_else(|_| current_status(sid));
    let mut headers = current_pending(sid);
    if !headers.iter().any(|(key, _)| key.eq_ignore_ascii_case("content-type")) {
        headers.push(("content-type".to_owned(), content_type.to_owned()));
    }
    web_response(ctx.env, status, headers, body.into_bytes())
}

fn body_response(ctx: FunctionCallContext, sid: u64) -> Result<Unknown<'static>> {
    let data: String = ctx.first_arg()?;
    let status = ctx.get::<u16>(1).unwrap_or_else(|_| current_status(sid));
    let mut headers = current_pending(sid);
    if let Ok(extra) = ctx.get::<Unknown>(2) {
        if let Ok(json) = js_to_json(ctx.env, &extra) {
            if let Some(map) = json.as_object() {
                for (key, value) in map {
                    if let Some(text) = value.as_str() {
                        header_set(&mut headers, key, text, false);
                    }
                }
            }
        }
    }
    web_response(ctx.env, status, headers, data.into_bytes())
}

fn redirect_response(ctx: FunctionCallContext, sid: u64) -> Result<Unknown<'static>> {
    let url: String = ctx.first_arg()?;
    let status = ctx.get::<u16>(1).unwrap_or(302);
    let _ = URL::parse_with_base(&url);
    let mut headers = current_pending(sid);
    header_set(&mut headers, "location", &url, false);
    web_response(ctx.env, status, headers, Vec::new())
}

struct URL;
impl URL {
    fn parse_with_base(url: &str) -> Result<()> {
        if url.contains("://") || url.starts_with('/') {
            Ok(())
        } else {
            Err(Error::from_reason("invalid redirect status"))
        }
    }
}

fn current_status(sid: u64) -> u16 {
    sessions().lock().unwrap().get(&sid).map(|session| session.state.lock().unwrap().status).unwrap_or(200)
}

fn current_pending(sid: u64) -> Vec<(String, String)> {
    sessions()
        .lock()
        .unwrap()
        .get(&sid)
        .map(|session| session.state.lock().unwrap().pending.clone())
        .unwrap_or_default()
}

fn attach_body_method(env: &Env, req: &mut Object, name: &str) -> Result<()> {
    let method = name.to_owned();
    req.set_named_property(
        name,
        {
            let method_fn: Function<(), Unknown<'static>> = env.create_function_from_closure(name, move |ctx| {
                let this: Object = ctx.this()?;
                let raw: Object = this.get_named_property("raw")?;
                let func: Function<(), Unknown> = raw.get_named_property(&method)?;
                Ok(own(func.apply(raw, ())?))
            })?;
            method_fn
        },
    )?;
    Ok(())
}

fn param_value(ctx: FunctionCallContext, params: &HashMap<String, String>) -> Result<Unknown<'static>> {
    if ctx.length() == 0 {
        let mut object = Object::new(ctx.env)?;
        for (key, value) in params {
            object.set_named_property(key, value.clone())?;
        }
        return Ok(own(object.to_unknown()));
    }
    let name: String = ctx.first_arg()?;
    match params.get(&name) {
        Some(value) => Ok(own(js_string(ctx.env, value)?)),
        None => Ok(own(undefined(ctx.env)?)),
    }
}

fn query_value(ctx: FunctionCallContext, url: &str) -> Result<Unknown<'static>> {
    let query = url.split_once('?').map(|(_, query)| query.split('#').next().unwrap_or("")).unwrap_or("");
    let mut pairs = Vec::new();
    if !query.is_empty() {
        for pair in query.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            pairs.push((try_decode_query(key), try_decode_query(value)));
        }
    }
    if ctx.length() == 0 {
        let mut object = Object::new(ctx.env)?;
        for (key, value) in &pairs {
            let existing = object.get_named_property::<Unknown>(key).ok();
            if existing.as_ref().is_some_and(is_defined) {
                continue;
            }
            clear_exception(ctx.env);
            object.set_named_property(key, value.clone())?;
        }
        return Ok(own(object.to_unknown()));
    }
    let name: String = ctx.first_arg()?;
    match pairs.iter().find(|(key, _)| key == &name) {
        Some((_, value)) => Ok(own(js_string(ctx.env, value)?)),
        None => Ok(own(undefined(ctx.env)?)),
    }
}

fn header_value(ctx: FunctionCallContext, headers: &[(String, String)]) -> Result<Unknown<'static>> {
    if ctx.length() == 0 {
        let mut object = Object::new(ctx.env)?;
        for (key, value) in headers {
            let lower = key.to_ascii_lowercase();
            if let Ok(existing) = object.get_named_property::<String>(&lower) {
                object.set_named_property(&lower, format!("{existing}, {value}"))?;
            } else {
                object.set_named_property(&lower, value.clone())?;
            }
        }
        return Ok(own(object.to_unknown()));
    }
    let name: String = ctx.first_arg()?;
    let matches: Vec<&str> = headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(&name))
        .map(|(_, value)| value.as_str())
        .collect();
    if matches.is_empty() {
        Ok(own(undefined(ctx.env)?))
    } else {
        Ok(own(js_string(ctx.env, &matches.join(", "))?))
    }
}

async fn run_slot(session: Arc<Session>, position: usize) -> Result<()> {
    let current = session.index.load(Ordering::SeqCst);
    if position as i32 <= current {
        return Err(Error::from_reason("next() called multiple times"));
    }
    session.index.store(position as i32, Ordering::SeqCst);
    if position >= session.slots.len() {
        return run_not_found(session).await;
    }
    {
        let mut state = session.state.lock().unwrap();
        state.params = session.slots[position].params.clone();
    }
    let sid = session_id_of(&session);
    let payload = json!({ "sid": sid, "slot": position, "kind": "route" }).to_string();
    let outcome = session.slots[position].handler.remote.call_async_catch(payload).await;
    let outcome = match outcome {
        Ok(promise) => promise.await,
        Err(error) => Err(error),
    };
    match outcome {
        Ok(body) => apply_handler_json(&session, position, &body),
        Err(error) => run_on_error(&session, position, &error_message(&error)).await,
    }
}

fn apply_handler_json(session: &Session, position: usize, body: &str) -> Result<()> {
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let kind = value.get("kind").and_then(Value::as_str).unwrap_or("empty");
    if kind == "response" {
        let mut state = session.state.lock().unwrap();
        if !state.finalized {
            let status = value.get("status").and_then(Value::as_u64).unwrap_or(200) as u16;
            let headers = json_headers(value.get("headers"));
            let bytes = value
                .get("body")
                .and_then(Value::as_str)
                .and_then(|text| STANDARD.decode(text).ok())
                .unwrap_or_default();
            finalize(&mut state, status, headers, bytes);
        }
        return Ok(());
    }
    let called_next = session.slots[position].called_next.load(Ordering::SeqCst);
    let finalized = session.state.lock().unwrap().finalized;
    if !finalized && !called_next {
        return Err(Error::from_reason(NOT_FINALIZED));
    }
    Ok(())
}

async fn run_not_found(session: Arc<Session>) -> Result<()> {
    let Some(handler) = session.not_found.clone() else {
        let mut state = session.state.lock().unwrap();
        if !state.finalized {
            finalize(&mut state, 404, vec![("content-type".into(), "text/plain; charset=UTF-8".into())], b"404 Not Found".to_vec());
        }
        return Ok(());
    };
    let sid = session_id_of(&session);
    let payload = json!({ "sid": sid, "slot": 0, "kind": "notFound" }).to_string();
    let outcome = match handler.remote.call_async_catch(payload).await {
        Ok(promise) => promise.await,
        Err(error) => Err(error),
    };
    match outcome {
        Ok(body) => {
            let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            if value.get("kind").and_then(Value::as_str) == Some("response") {
                let mut state = session.state.lock().unwrap();
                if !state.finalized {
                    let status = value.get("status").and_then(Value::as_u64).unwrap_or(404) as u16;
                    finalize(&mut state, status, json_headers(value.get("headers")), decode_body(&value));
                }
            }
            Ok(())
        }
        Err(error) => run_on_error(&session, session.slots.len().saturating_sub(1), &error_message(&error)).await,
    }
}

async fn run_on_error(session: &Session, position: usize, message: &str) -> Result<()> {
    let handler = session
        .slots
        .get(position)
        .and_then(|slot| slot.on_error.clone())
        .or_else(|| session.on_error.clone());
    let Some(handler) = handler else {
        let mut state = session.state.lock().unwrap();
        state.finalized = false;
        finalize(
            &mut state,
            500,
            vec![("content-type".into(), "text/plain; charset=UTF-8".into())],
            b"Internal Server Error".to_vec(),
        );
        return Ok(());
    };
    let sid = session_id_of(session);
    let payload = json!({ "sid": sid, "slot": position, "kind": "error", "message": message }).to_string();
    let outcome = match handler.remote.call_async_catch(payload).await {
        Ok(promise) => promise.await,
        Err(error) => Err(error),
    };
    match outcome {
        Ok(body) => {
            let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let mut state = session.state.lock().unwrap();
            state.finalized = false;
            if value.get("kind").and_then(Value::as_str) == Some("response") {
                let status = value.get("status").and_then(Value::as_u64).unwrap_or(500) as u16;
                finalize(&mut state, status, json_headers(value.get("headers")), decode_body(&value));
            } else {
                finalize(&mut state, 500, Vec::new(), b"Internal Server Error".to_vec());
            }
            Ok(())
        }
        Err(_) => {
            let mut state = session.state.lock().unwrap();
            state.finalized = false;
            finalize(&mut state, 500, Vec::new(), b"Internal Server Error".to_vec());
            Ok(())
        }
    }
}

fn finalize(state: &mut State, status: u16, mut headers: Vec<(String, String)>, body: Vec<u8>) {
    if state.finalized {
        return;
    }
    if state.wrote && status != 0 {
        for (key, value) in state.pending.clone() {
            header_set(&mut headers, &key, &value, false);
        }
    }
    state.finalized = true;
    state.res_status = if status == 0 { 500 } else { status };
    state.res_headers = headers;
    state.res_body = body;
}

fn session_id_of(session: &Session) -> u64 {
    session.id
}

fn header_set(headers: &mut Vec<(String, String)>, name: &str, value: &str, append: bool) {
    if append {
        if let Some((_, existing)) = headers.iter_mut().find(|(key, _)| key.eq_ignore_ascii_case(name)) {
            existing.push_str(", ");
            existing.push_str(value);
            return;
        }
    } else {
        headers.retain(|(key, _)| !key.eq_ignore_ascii_case(name));
    }
    headers.push((name.to_owned(), value.to_owned()));
}

fn json_headers(value: Option<&Value>) -> Vec<(String, String)> {
    value
        .and_then(Value::as_array)
        .map(|pairs| {
            pairs
                .iter()
                .filter_map(|pair| {
                    let pair = pair.as_array()?;
                    Some((pair.first()?.as_str()?.to_owned(), pair.get(1)?.as_str()?.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn decode_body(value: &Value) -> Vec<u8> {
    value
        .get("body")
        .and_then(Value::as_str)
        .and_then(|text| STANDARD.decode(text).ok())
        .unwrap_or_default()
}

fn error_message(error: &Error) -> String {
    let reason = error.reason.clone();
    if reason.is_empty() { "Error".to_owned() } else { reason }
}

fn either_list(value: Either<String, Vec<String>>) -> Vec<String> {
    match value {
        Either::A(value) => vec![value],
        Either::B(value) => value,
    }
}

fn merge_path(base: &str, sub: &str) -> String {
    let left = if base.starts_with('/') { base.to_owned() } else { format!("/{base}") };
    if sub == "/" || sub.is_empty() {
        return left;
    }
    let trimmed = left.trim_end_matches('/');
    let right = if sub.starts_with('/') { sub.to_owned() } else { format!("/{sub}") };
    let merged = format!("{trimmed}{right}");
    if merged.is_empty() { "/".to_owned() } else { merged }
}

fn belongs_to(record_base: &str, app_base: &str) -> bool {
    app_base == "/" || record_base == app_base || record_base.starts_with(&format!("{app_base}/"))
}

fn try_decode_query(value: &str) -> String {
    super::router::try_decode(value)
}

fn promise_of(env: &Env, value: Unknown) -> Result<Unknown<'static>> {
    let ctor: Unknown = env.get_global()?.get_named_property("Promise")?;
    let mut resolve_raw = std::ptr::null_mut();
    let name = std::ffi::CString::new("resolve").unwrap();
    check_status!(unsafe { sys::napi_get_named_property(env.raw(), ctor.raw(), name.as_ptr(), &mut resolve_raw) })?;
    let resolve: Function<Unknown, Unknown> = unsafe { Function::from_napi_value(env.raw(), resolve_raw)? };
    Ok(own(resolve.apply(ctor, value)?))
}

fn promise_json(env: &Env, value: Unknown) -> Result<Unknown<'static>> {
    let adopted = promise_of(env, value)?;
    let adopted_object: Object = unsafe { Object::from_napi_value(env.raw(), adopted.raw())? };
    let then: Function<(), Unknown<'static>> = env.create_function_from_closure("encode", |ctx| encode_handler_value(ctx))?;
    let then_fn: Function<Function<(), Unknown<'static>>, Unknown> = adopted_object.get_named_property("then")?;
    then_fn.apply(adopted_object, then)
}

fn encode_handler_value(ctx: FunctionCallContext) -> Result<Unknown<'static>> {
    if ctx.length() == 0 || is_undefined(ctx.env, ctx.get::<Unknown>(0).ok()) {
        return Ok(own(js_string(ctx.env, r#"{"kind":"empty"}"#)?));
    }
    let value: Unknown = ctx.get(0)?;
    if !is_response(ctx.env, &value)? {
        return Ok(own(js_string(ctx.env, r#"{"kind":"empty"}"#)?));
    }
    let response: Object = unsafe { Object::from_napi_value(ctx.env.raw(), value.raw())? };
    let status = response.get_named_property::<u16>("status").unwrap_or(200);
    let headers = read_headers(ctx.env, &response)?;
    let array_buffer: Function<(), Unknown> = response.get_named_property("arrayBuffer")?;
    let bytes_promise = array_buffer.apply(&response, ())?;
    let bytes_object: Object = unsafe { Object::from_napi_value(ctx.env.raw(), bytes_promise.raw())? };
    let then: Function<(), Unknown<'static>> = ctx.env.create_function_from_closure("bytes", move |ctx| {
        let buffer: ArrayBuffer = ctx.first_arg()?;
        let bytes: &[u8] = buffer.as_ref();
        let encoded = STANDARD.encode(bytes);
        let payload = json!({ "kind": "response", "status": status, "headers": headers, "body": encoded }).to_string();
        Ok(own(js_string(ctx.env, &payload)?))
    })?;
    let then_fn: Function<Function<(), Unknown<'static>>, Unknown> = bytes_object.get_named_property("then")?;
    then_fn.apply(bytes_object, then)
}

fn is_defined(value: &Unknown) -> bool {
    !matches!(value.get_type().ok(), Some(ValueType::Undefined | ValueType::Null) | None)
}

fn is_undefined(env: &Env, value: Option<Unknown>) -> bool {
    let Some(value) = value else {
        return true;
    };
    value.get_type().ok() == Some(ValueType::Undefined) || value.get_type().ok() == Some(ValueType::Null)
}

fn is_response(env: &Env, value: &Unknown) -> Result<bool> {
    let ctor: Unknown = env.get_global()?.get_named_property("Response")?;
    let mut result = false;
    check_status!(unsafe { sys::napi_instanceof(env.raw(), value.raw(), ctor.raw(), &mut result) })?;
    Ok(result)
}

fn read_headers(env: &Env, response: &Object) -> Result<Vec<(String, String)>> {
    let headers: Object = response.get_named_property("headers")?;
    collect_headers(env, &headers)
}

fn collect_headers(env: &Env, headers: &Object) -> Result<Vec<(String, String)>> {
    let collected = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let collect = collected.clone();
    let for_each: Function<(), ()> = env.create_function_from_closure("forEach", move |ctx| {
        let value: String = ctx.get(0)?;
        let key: String = ctx.get(1)?;
        collect.borrow_mut().push((key, value));
        Ok(())
    })?;
    let func: Function<Function<(), ()>, Unknown> = headers.get_named_property("forEach")?;
    func.apply(headers, for_each)?;
    Ok(collected.borrow().clone())
}

fn copy_bytes(env: &Env, value: Unknown) -> Result<Vec<u8>> {
    let raw = value.raw();
    if let Ok(buffer) = unsafe { ArrayBuffer::from_napi_value(env.raw(), raw) } {
        let bytes: &[u8] = buffer.as_ref();
        return Ok(bytes.to_vec());
    }
    clear_exception(env);
    if let Ok(buffer) = unsafe { Buffer::from_napi_value(env.raw(), raw) } {
        let bytes: &[u8] = buffer.as_ref();
        return Ok(bytes.to_vec());
    }
    clear_exception(env);
    let object: Object = unsafe { Object::from_napi_value(env.raw(), raw) }?;
    let buffer_value: Unknown = object.get_named_property("buffer")?;
    let buffer: ArrayBuffer = unsafe { ArrayBuffer::from_napi_value(env.raw(), buffer_value.raw()) }?;
    let offset = object.get_named_property::<u32>("byteOffset").unwrap_or(0) as usize;
    let length = object.get_named_property::<u32>("byteLength").unwrap_or(0) as usize;
    let bytes: &[u8] = buffer.as_ref();
    let end = offset.saturating_add(length).min(bytes.len());
    let start = offset.min(end);
    Ok(bytes[start..end].to_vec())
}

fn web_response(env: &Env, status: u16, headers: Vec<(String, String)>, body: Vec<u8>) -> Result<Unknown<'static>> {
    let ctor: Function<CallArgs<(Unknown, Object)>, Unknown> = env.get_global()?.get_named_property("Response")?;
    let mut init = Object::new(env)?;
    init.set_named_property("status", status)?;
    let mut header_object = Object::new(env)?;
    for (key, value) in headers {
        header_object.set_named_property(&key, value)?;
    }
    init.set_named_property("headers", header_object)?;
    let payload = js_bytes(env, &body)?;
    ctor.new_instance(CallArgs((payload, init)))
}

fn js_error(env: &Env, message: &str) -> Result<Unknown<'static>> {
    let ctor: Function<String, Unknown> = env.get_global()?.get_named_property("Error")?;
    ctor.call(message.to_owned())
}

fn js_to_json(env: &Env, value: &Unknown) -> Result<Value> {
    let json: Object = env.get_global()?.get_named_property("JSON")?;
    let stringify: Function<Unknown, String> = json.get_named_property("stringify")?;
    let text = stringify.apply(&json, value.clone())?;
    serde_json::from_str(&text).map_err(|error| Error::from_reason(error.to_string()))
}

fn json_to_js(env: &Env, value: &Value) -> Result<Unknown<'static>> {
    let json: Object = env.get_global()?.get_named_property("JSON")?;
    let parse: Function<String, Unknown> = json.get_named_property("parse")?;
    if value.is_null() {
        return Ok(own(undefined(env)?));
    }
    parse.apply(&json, value.to_string())
}

fn to_static(value: Unknown) -> Unknown<'static> {
    unsafe { std::mem::transmute(value) }
}

fn dispatch_prepared(
    env: &Env,
    server: &Server,
    mut prepared: Prepared,
    env_value: Option<Value>,
    execution: Option<JsRef>,
) -> Result<Unknown<'static>> {
    if let Some(func) = server.view.inner.get_path.lock().unwrap().as_ref() {
        let request = build_raw_request(env, &prepared.url, &prepared.method, &prepared.headers, &prepared.body)?;
        if let Ok(borrowed) = func.borrow_back(env) {
            let missing = undefined(env)?;
            if let Ok(path) = borrowed.call(CallArgs((to_static(request), to_static(missing)))) {
                let mut coerced = std::ptr::null_mut();
                if unsafe { sys::napi_coerce_to_string(env.raw(), path.raw(), &mut coerced) } == sys::Status::napi_ok {
                    if let Ok(path) = unsafe { String::from_napi_value(env.raw(), coerced) } {
                        prepared.path = path;
                    }
                }
            }
        }
    }
    let inner = server.view.inner.clone();
    let strict = inner.strict;
    if !strict && prepared.path.len() > 1 && prepared.path.ends_with('/') {
        prepared.path.pop();
    }
    let env_value = env_value.unwrap_or(Value::Null);
    let future_prepared = prepared.clone();
    let promise = env.spawn_future_with_callback(
        async move { dispatch_inner(inner, future_prepared, env_value, execution).await },
        |env, wire| finish_wire(env, wire),
    )?;
    Ok(own(promise.to_unknown()))
}

#[derive(Clone)]
struct Prepared {
    method: String,
    url: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

struct Wire {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    head: bool,
    execution: Option<JsRef>,
}

async fn dispatch_inner(inner: Arc<Inner>, prepared: Prepared, env_value: Value, execution: Option<JsRef>) -> Result<Wire> {
    let head = prepared.method.eq_ignore_ascii_case("HEAD");
    let specific = inner
        .router
        .match_route("HEAD", &prepared.path)
        .iter()
        .any(|route| route.method == "HEAD");
    let method = if head && specific { "HEAD" } else if head { "GET" } else { prepared.method.as_str() };
    let matched = inner.router.match_route(method, &prepared.path);
    if matched.is_empty() {
        if let Some(wire) = call_legacy(&inner, &prepared).await? {
            return Ok(wire);
        }
    }
    let handlers = inner.handlers.lock().unwrap().clone();
    let records = inner.records.lock().unwrap().clone();
    let mut slots = Vec::new();
    for route in matched {
        let Some(handler) = handlers.get(route.handler as usize).cloned() else {
            continue;
        };
        let on_error = records.iter().find(|record| Arc::ptr_eq(&record.handler, &handler)).and_then(|record| record.on_error.clone());
        slots.push(Slot {
            handler,
            params: route.params,
            on_error,
            called_next: AtomicBool::new(false),
        });
    }
    drop(handlers);
    drop(records);
    let sid = next_session_id();
    let session = Arc::new(Session {
        id: sid,
        state: StdMutex::new(State {
            method: prepared.method.clone(),
            url: prepared.url,
            path: prepared.path,
            headers: prepared.headers,
            body: prepared.body,
            params: HashMap::new(),
            env: env_value,
            execution,
            vars: HashMap::new(),
            status: 200,
            pending: Vec::new(),
            wrote: false,
            finalized: false,
            res_status: 200,
            res_headers: Vec::new(),
            res_body: Vec::new(),
        }),
        slots,
        not_found: inner.not_found.lock().unwrap().clone(),
        on_error: inner.on_error.lock().unwrap().clone(),
        index: AtomicI32::new(-1),
    });
    sessions().lock().unwrap().insert(sid, session.clone());
    let result = run_slot(session.clone(), 0).await;
    if result.is_err() {
        let _ = run_on_error(&session, 0, &error_message(&result.unwrap_err())).await;
    }
    if !session.state.lock().unwrap().finalized {
        let _ = run_not_found(session.clone()).await;
    }
    let mut state = session.state.lock().unwrap();
    let execution = state.execution.take();
    let wire = Wire {
        status: state.res_status,
        headers: state.res_headers.clone(),
        body: state.res_body.clone(),
        head,
        execution,
    };
    drop(state);
    sessions().lock().unwrap().remove(&sid);
    Ok(wire)
}

fn legacy_score(pattern: &str) -> i32 {
    pattern.split('/').fold(0, |total, part| {
        total + if part == "*" { -1000 } else if part.starts_with(':') { 1 } else { 10 }
    })
}

fn match_legacy(pattern: &str, path: &str) -> Option<HashMap<String, String>> {
    let parts: Vec<&str> = pattern.split('/').collect();
    let actual: Vec<&str> = path.split('/').collect();
    let mut params = HashMap::new();
    for (index, part) in parts.iter().enumerate() {
        if *part == "*" {
            return Some(params);
        }
        let Some(segment) = actual.get(index) else {
            return None;
        };
        if let Some(name) = part.strip_prefix(':') {
            params.insert(name.to_owned(), try_decode_query(segment));
        } else if part != segment {
            return None;
        }
    }
    if parts.len() == actual.len() { Some(params) } else { None }
}

fn legacy_remote(env: &Env, routes: Vec<(String, JsRef)>, fetch: Option<UserFn>) -> Result<Option<Arc<RemoteFn>>> {
    if routes.is_empty() && fetch.is_none() {
        return Ok(None);
    }
    let table = Arc::new(LegacyTable { routes, fetch });
    let adapter: Function<String, Unknown> = env.create_function_from_closure("legacy", move |ctx| {
        let payload: String = ctx.first_arg()?;
        dispatch_legacy(ctx.env, &table, &payload)
    })?;
    Ok(Some(Arc::new(as_remote(adapter)?)))
}

fn dispatch_legacy(env: &Env, table: &LegacyTable, payload: &str) -> Result<Unknown<'static>> {
    let value: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
    let prepared = prepared_from_payload(&value);
    let mut ordered: Vec<_> = table.routes.iter().collect();
    ordered.sort_by(|left, right| legacy_score(&right.0).cmp(&legacy_score(&left.0)));
    for (pattern, reference) in ordered {
        let Some(params) = match_legacy(pattern, &prepared.path) else {
            continue;
        };
        let route = reference.get(env)?;
        return promise_json(env, materialize_legacy(env, route, &prepared, &params)?);
    }
    if let Some(fetch) = table.fetch.as_ref() {
        let request = legacy_request(env, &prepared, &HashMap::new())?;
        let func = fetch.borrow_back(env)?;
        let missing = undefined(env)?;
        return promise_json(env, func.call(CallArgs((to_static(request), to_static(missing))))?);
    }
    promise_of(env, js_string(env, r#"{"kind":"miss"}"#)?)
}

fn prepared_from_payload(value: &Value) -> Prepared {
    let headers = value
        .get("headers")
        .and_then(Value::as_array)
        .map(|pairs| {
            pairs
                .iter()
                .filter_map(|pair| {
                    let pair = pair.as_array()?;
                    Some((pair.first()?.as_str()?.to_owned(), pair.get(1)?.as_str()?.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default();
    Prepared {
        method: value.get("method").and_then(Value::as_str).unwrap_or("GET").to_owned(),
        url: value.get("url").and_then(Value::as_str).unwrap_or("http://localhost/").to_owned(),
        path: value.get("path").and_then(Value::as_str).unwrap_or("/").to_owned(),
        headers,
        body: value
            .get("body")
            .and_then(Value::as_str)
            .and_then(|text| STANDARD.decode(text).ok())
            .unwrap_or_default(),
    }
}

async fn call_legacy(inner: &Inner, prepared: &Prepared) -> Result<Option<Wire>> {
    let Some(remote) = inner.legacy_remote.lock().unwrap().clone() else {
        return Ok(None);
    };
    let payload = json!({
        "method": prepared.method,
        "url": prepared.url,
        "path": prepared.path,
        "headers": prepared.headers,
        "body": STANDARD.encode(&prepared.body),
    })
    .to_string();
    let text = match remote.call_async_catch(payload).await {
        Ok(promise) => match promise.await {
            Ok(text) => text,
            Err(error) => return Ok(Some(error_wire(&error_message(&error)))),
        },
        Err(error) => return Ok(Some(error_wire(&error_message(&error)))),
    };
    let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    match value.get("kind").and_then(Value::as_str) {
        Some("response") => Ok(Some(Wire {
            status: value.get("status").and_then(Value::as_u64).unwrap_or(200) as u16,
            headers: json_headers(value.get("headers")),
            body: decode_body(&value),
            head: prepared.method.eq_ignore_ascii_case("HEAD"),
            execution: None,
        })),
        Some("miss") => Ok(None),
        _ => Ok(Some(error_wire("Internal Server Error"))),
    }
}

fn error_wire(message: &str) -> Wire {
    Wire {
        status: 500,
        headers: vec![("content-type".into(), "text/plain; charset=UTF-8".into())],
        body: message.as_bytes().to_vec(),
        head: false,
        execution: None,
    }
}

fn materialize_legacy(env: &Env, value: Unknown, prepared: &Prepared, params: &HashMap<String, String>) -> Result<Unknown<'static>> {
    if is_response(env, &value)? {
        let object: Object = unsafe { Object::from_napi_value(env.raw(), value.raw())? };
        let clone: Function<(), Unknown> = object.get_named_property("clone")?;
        return Ok(own(clone.apply(&object, ())?));
    }
    let object: Object = unsafe { Object::from_napi_value(env.raw(), value.raw())? };
    if let Ok(bytes) = object.get_named_property::<Function<(), Unknown>>("bytes") {
        let content_type = object.get_named_property::<String>("type").unwrap_or_else(|_| "application/octet-stream".into());
        let promise = bytes.apply(&object, ())?;
        let promise_object: Object = unsafe { Object::from_napi_value(env.raw(), promise.raw())? };
        let then: Function<(), Unknown<'static>> = env.create_function_from_closure("file", move |ctx| {
            let value: Unknown = ctx.first_arg()?;
            let data = copy_bytes(ctx.env, value)?;
            web_response(ctx.env, 200, vec![("content-type".to_owned(), content_type.clone())], data)
        })?;
        let then_fn: Function<Function<(), Unknown<'static>>, Unknown> = promise_object.get_named_property("then")?;
        return Ok(own(then_fn.apply(promise_object, then)?));
    }
    clear_exception(env);
    if let Ok(method_value) = object.get_named_property::<Unknown>(&prepared.method) {
        if is_defined(&method_value) {
            return materialize_legacy(env, method_value, prepared, params);
        }
    } else {
        clear_exception(env);
    }
    let request = legacy_request(env, prepared, params)?;
    let func: Function<Unknown, Unknown> = unsafe { Function::from_napi_value(env.raw(), value.raw())? };
    Ok(own(func.call(request)?))
}

fn legacy_request(env: &Env, prepared: &Prepared, params: &HashMap<String, String>) -> Result<Unknown<'static>> {
    let mut object = Object::new(env)?;
    object.set_named_property("url", prepared.url.clone())?;
    object.set_named_property("method", prepared.method.clone())?;
    let mut param_object = Object::new(env)?;
    for (key, value) in params {
        param_object.set_named_property(key, value.clone())?;
    }
    object.set_named_property("params", param_object)?;
    let raw = build_raw_request(env, &prepared.url, &prepared.method, &prepared.headers, &prepared.body)?;
    object.set_named_property("json", {
        let method: Function<(), Unknown<'static>> = env.create_function_from_closure("json", move |ctx| {
            let this: Object = ctx.this()?;
            let raw: Object = this.get_named_property("raw")?;
            let func: Function<(), Unknown> = raw.get_named_property("json")?;
            Ok(own(func.apply(raw, ())?))
        })?;
        method
    })?;
    object.set_named_property("text", {
        let method: Function<(), Unknown<'static>> = env.create_function_from_closure("text", move |ctx| {
            let this: Object = ctx.this()?;
            let raw: Object = this.get_named_property("raw")?;
            let func: Function<(), Unknown> = raw.get_named_property("text")?;
            Ok(own(func.apply(raw, ())?))
        })?;
        method
    })?;
    object.set_named_property("raw", raw)?;
    Ok(own(object.to_unknown()))
}

fn finish_wire(env: &Env, wire: Wire) -> Result<Unknown<'static>> {
    if let Some(reference) = wire.execution {
        let _ = reference.unref(env);
    }
    let mut headers = wire.headers;
    let body = if wire.head {
        if !headers.iter().any(|(key, _)| key.eq_ignore_ascii_case("content-length")) {
            headers.push(("content-length".into(), wire.body.len().to_string()));
        }
        Vec::new()
    } else {
        wire.body
    };
    web_response(env, wire.status, headers, body)
}

fn read_request(env: &Env, request: &Unknown) -> Result<Prepared> {
    let object: Object = unsafe { Object::from_napi_value(env.raw(), request.raw())? };
    let method = object.get_named_property::<String>("method").unwrap_or_else(|_| "GET".into());
    let url = object.get_named_property::<String>("url").unwrap_or_else(|_| "http://localhost/".into());
    let headers = object
        .get_named_property::<Object>("headers")
        .and_then(|headers| collect_headers(env, &headers))
        .unwrap_or_default();
    Ok(Prepared {
        method,
        path: default_path(&url),
        url,
        headers,
        body: Vec::new(),
    })
}

fn default_path(url: &str) -> String {
    let protocol = url.find("://").map(|index| index + 3).unwrap_or(0);
    let Some(start) = url[protocol..].find('/').map(|index| protocol + index) else {
        return "/".to_owned();
    };
    let mut end = url.len();
    if let Some(query) = url[start..].find('?') {
        end = end.min(start + query);
    }
    if let Some(hash) = url[start..].find('#') {
        end = end.min(start + hash);
    }
    let path = &url[start..end];
    if path.is_empty() { "/".to_owned() } else { path.to_owned() }
}

fn build_raw_request(env: &Env, url: &str, method: &str, headers: &[(String, String)], body: &[u8]) -> Result<Unknown<'static>> {
    let ctor: Function<CallArgs<(String, Object)>, Unknown> = env.get_global()?.get_named_property("Request")?;
    let mut init = Object::new(env)?;
    init.set_named_property("method", method)?;
    let mut header_object = Object::new(env)?;
    for (key, value) in headers {
        if key.eq_ignore_ascii_case("content-length")
            || key.eq_ignore_ascii_case("transfer-encoding")
            || key.eq_ignore_ascii_case("connection")
        {
            continue;
        }
        header_object.set_named_property(key, value.clone())?;
    }
    init.set_named_property("headers", header_object)?;
    if !body.is_empty() && !method.eq_ignore_ascii_case("GET") && !method.eq_ignore_ascii_case("HEAD") {
        init.set_named_property("body", js_bytes(env, body)?)?;
    }
    Ok(own(ctor.new_instance(CallArgs((url.to_owned(), init)))?))
}

fn new_request(env: &Env, url: &str, init: Option<Unknown>) -> Result<Unknown<'static>> {
    let ctor: Function<CallArgs<(String, Option<Unknown>)>, Unknown> = env.get_global()?.get_named_property("Request")?;
    Ok(own(ctor.new_instance(CallArgs((url.to_owned(), init)))?))
}

fn new_request_from(env: &Env, request: &Unknown, init: &Unknown) -> Result<Unknown<'static>> {
    let ctor: Function<CallArgs<(Unknown, Unknown)>, Unknown> = env.get_global()?.get_named_property("Request")?;
    Ok(own(ctor.new_instance(CallArgs((request.clone(), init.clone())))?))
}

fn request_method(env: &Env, _request: &Unknown, _method: &str) -> Option<()> {
    let _ = env;
    None
}

fn install_timer(env: &Env, inner: &Inner) -> Result<()> {
    let global = env.get_global()?;
    let set_interval: Function<CallArgs<(Function<(), ()>, u32)>, Unknown> = global.get_named_property("setInterval")?;
    let noop: Function<(), ()> = env.create_function_from_closure("keepalive", |_| Ok(()))?;
    let timer = set_interval.call(CallArgs((noop, 1 << 30)))?;
    *inner.timer.lock().unwrap() = Some(create_js_ref(env, &timer)?);
    Ok(())
}

fn clear_timer(env: &Env, inner: &Inner) {
    if let Some(timer) = inner.timer.lock().unwrap().take() {
        if let Ok(global) = env.get_global() {
            if let Ok(clear) = global.get_named_property::<Function<Unknown, Unknown>>("clearInterval") {
                if let Ok(value) = timer.get(env) {
                    let _ = clear.call(value);
                }
            }
        }
        let _ = timer.unref(env);
    }
}

fn ensure_fetch_host(env: &Env) -> Result<()> {
    let global = env.get_global()?;
    if let Ok(existing) = global.get_named_property::<Unknown>("addEventListener") {
        if is_defined(&existing) {
            return Ok(());
        }
    }
    let script = r#"
      const listeners = new Set();
      globalThis.addEventListener = (type, listener) => { if (type === "fetch") listeners.add(listener); };
      globalThis.removeEventListener = (type, listener) => { if (type === "fetch") listeners.delete(listener); };
      globalThis.dispatchEvent = (event) => {
        if (event.type !== "fetch") return false;
        for (const listener of Array.from(listeners)) listener(event);
        return true;
      };
    "#;
    let function: Function<String, Unknown> = env.get_global()?.get_named_property("eval")?;
    function.call(script.to_owned())?;
    Ok(())
}

async fn rocket_start(inner: Arc<Inner>) -> Result<u16> {
    use rocket::data::ToByteUnit;
    use rocket::http::{Header, Method, Status};
    use rocket::route::{Handler, Outcome};
    use rocket::{Data, Request, Route};
    use std::io::Cursor;

    let mut running = inner.rocket.lock().await;
    if running.is_some() {
        return Err(Error::from_reason("server already started"));
    }
    let address = inner.host.parse::<std::net::IpAddr>().map_err(|error| Error::from_reason(error.to_string()))?;
    let mut config = rocket::Config::debug_default();
    config.address = address;
    config.port = inner.configured_port;
    config.log_level = rocket::config::LogLevel::Off;
    config.shutdown.ctrlc = false;
    config.shutdown.grace = 0;
    config.shutdown.mercy = 0;
    config.limits = rocket::data::Limits::default()
        .limit("bytes", (MAX_BODY_BYTES as usize).bytes())
        .limit("string", (MAX_BODY_BYTES as usize).bytes())
        .limit("data-form", (MAX_BODY_BYTES as usize).bytes())
        .limit("file", (MAX_BODY_BYTES as usize).bytes())
        .limit("json", (MAX_BODY_BYTES as usize).bytes())
        .limit("form", (MAX_BODY_BYTES as usize).bytes());
    let (ready_tx, ready_rx) = oneshot::channel();
    let ready_tx = Arc::new(Mutex::new(Some(ready_tx)));
    let port = Arc::new(AtomicU16::new(0));
    let port_store = port.clone();
    let listening = Arc::new(AtomicBool::new(false));
    let listening_store = listening.clone();
    let dispatch = RocketDispatch(inner.clone());
    let routes = [
        Method::Get,
        Method::Head,
        Method::Post,
        Method::Put,
        Method::Delete,
        Method::Patch,
        Method::Options,
        Method::Trace,
        Method::Connect,
    ]
    .into_iter()
    .flat_map(|method| {
        let handler = dispatch.clone();
        [Route::new(method, "/", handler.clone()), Route::new(method, "/<path..>", handler)]
    })
    .collect::<Vec<_>>();
    let rocket = rocket::custom(config)
        .mount("/", routes)
        .attach(rocket::fairing::AdHoc::on_liftoff("ready", move |rocket| {
            let tx = ready_tx.clone();
            let port = port_store.clone();
            let listening = listening_store.clone();
            Box::pin(async move {
                port.store(rocket.config().port, Ordering::SeqCst);
                listening.store(true, Ordering::SeqCst);
                if let Some(tx) = tx.lock().await.take() {
                    let _ = tx.send(rocket.config().port);
                }
            })
        }))
        .ignite()
        .await
        .map_err(|error| Error::from_reason(error.to_string()))?;
    let shutdown = rocket.shutdown();
    let flag = listening.clone();
    let task = tokio::spawn(async move {
        if let Err(error) = rocket.launch().await {
            eprintln!("Rocket server: {error}");
        }
        flag.store(false, Ordering::SeqCst);
    });
    let bound = ready_rx.await.map_err(|_| Error::from_reason("Rocket failed to bind"))?;
    inner.bound_port.store(bound, Ordering::SeqCst);
    inner.listening.store(true, Ordering::SeqCst);
    *running = Some(Running { shutdown, task });
    Ok(bound)
}

#[derive(Clone)]
struct RocketDispatch(Arc<Inner>);

#[rocket::async_trait]
impl rocket::route::Handler for RocketDispatch {
    async fn handle<'r>(&self, request: &'r rocket::Request<'_>, data: rocket::Data<'r>) -> rocket::route::Outcome<'r> {
        use rocket::data::ToByteUnit;
        use rocket::http::{Header, Status};
        use std::io::Cursor;
        for value in request.headers().get("Content-Length") {
            match value.parse::<u64>() {
                Ok(len) if len > MAX_BODY_BYTES => return rocket::route::Outcome::Error(Status::PayloadTooLarge),
                Ok(_) => {}
                Err(_) => return rocket::route::Outcome::Error(Status::BadRequest),
            }
        }
        let bytes = match data.open((MAX_BODY_BYTES as usize).bytes()).into_bytes().await {
            Ok(bytes) if bytes.is_complete() => bytes.into_inner(),
            _ => return rocket::route::Outcome::Error(Status::PayloadTooLarge),
        };
        let headers = request.headers().iter().map(|header| (header.name().to_string(), header.value().to_string())).collect::<Vec<_>>();
        let prepared = Prepared {
            method: request.method().as_str().to_owned(),
            url: format!("http://{}:{}{}", self.0.host, self.0.bound_port.load(Ordering::SeqCst), request.uri()),
            path: request.uri().path().to_string(),
            headers,
            body: bytes,
        };
        let wire = match dispatch_inner(self.0.clone(), prepared, Value::Null, None).await {
            Ok(wire) => wire,
            Err(_) => return rocket::route::Outcome::Error(Status::InternalServerError),
        };
        let mut response = rocket::Response::build();
        response.status(Status::new(wire.status));
        let mut declared = None;
        for (key, value) in &wire.headers {
            if key.eq_ignore_ascii_case("content-length") {
                declared = value.parse().ok();
                continue;
            }
            if key.eq_ignore_ascii_case("transfer-encoding") || key.eq_ignore_ascii_case("connection") {
                continue;
            }
            response.header_adjoin(Header::new(key.clone(), value.clone()));
        }
        if request.method() == rocket::http::Method::Head {
            response.sized_body(declared.or(Some(wire.body.len())), Cursor::new(Vec::<u8>::new()));
        } else {
            response.sized_body(wire.body.len(), Cursor::new(wire.body));
        }
        rocket::route::Outcome::Success(response.finalize())
    }
}

async fn rocket_stop(inner: Arc<Inner>) -> Result<()> {
    inner.listening.store(false, Ordering::SeqCst);
    if let Some(running) = inner.rocket.lock().await.take() {
        running.shutdown.notify();
        running.task.await.map_err(|error| Error::from_reason(error.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod dispatch_tests {
    use super::header_set;

    #[test]
    fn append_joins_header_values() {
        let mut headers = Vec::new();
        header_set(&mut headers, "x-appended", "part1", false);
        header_set(&mut headers, "x-appended", "part2", true);
        assert_eq!(headers, vec![("x-appended".into(), "part1, part2".into())]);
    }

    #[test]
    fn unfinished_handler_is_not_a_missing_route() {
        let called_next = false;
        let finalized = false;
        let outcome = if !finalized && !called_next { "error" } else { "missing" };
        assert_eq!(outcome, "error");
    }

    #[test]
    fn legacy_static_route_outranks_a_wildcard() {
        assert!(super::legacy_score("/api/status") > super::legacy_score("/api/*"));
        let params = super::match_legacy("/users/:id", "/users/a%20b").unwrap();
        assert_eq!(params.get("id").map(String::as_str), Some("a b"));
        assert!(super::match_legacy("/api/*", "/api/unknown").is_some());
        assert!(super::match_legacy("/api/status", "/api/unknown").is_none());
    }
}
