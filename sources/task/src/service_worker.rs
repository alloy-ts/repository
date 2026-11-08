use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use napi::bindgen_prelude::{
    Either, FromNapiValue, Function, FunctionRef, JsObjectValue, JsValue, Object, ToNapiValue,
    Unknown,
};
use napi::{Env, Error, JsDeferred, Result, Status};
use napi_derive::napi;

use super::{Task, TaskOptions};

/// A JS event handler, kept alive by the worker.
type SharedHandler = Arc<FunctionRef<Unknown<'static>, Unknown<'static>>>;

/// Handlers registered for a worker: dedicated `on*` property handlers and `addEventListener` listeners.
struct WorkerHandlers {
    on_handlers: HashMap<String, Option<SharedHandler>>,
    listeners: HashMap<String, Vec<SharedHandler>>,
}

impl WorkerHandlers {
    fn new() -> Self {
        Self {
            on_handlers: HashMap::new(),
            listeners: HashMap::new(),
        }
    }
}

type SharedWorkerHandlers = Arc<Mutex<WorkerHandlers>>;

/// Resolver used to settle `update()` or async operations.
type DeferredBool = Box<dyn FnOnce(Env) -> Result<bool> + Send>;

/// ServiceWorker lifecycle states conforming to MDN ServiceWorker.state.
#[napi(string_enum)]
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ServiceWorkerState {
    #[napi(value = "parsed")]
    Parsed,
    #[napi(value = "installing")]
    Installing,
    #[napi(value = "installed")]
    Installed,
    #[napi(value = "activating")]
    Activating,
    #[napi(value = "activated")]
    Activated,
    #[napi(value = "redundant")]
    Redundant,
}

impl ServiceWorkerState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Parsed => "parsed",
            Self::Installing => "installing",
            Self::Installed => "installed",
            Self::Activating => "activating",
            Self::Activated => "activated",
            Self::Redundant => "redundant",
        }
    }
}

/// Options for configuring a [`ServiceWorker`].
#[napi(object)]
#[derive(Default, Clone)]
pub struct ServiceWorkerOptions {
    /// Scope URL for the service worker registration.
    pub scope: Option<String>,
    /// Script evaluation type ("classic" or "module").
    pub r#type: Option<String>,
}

/// The ServiceWorker interface of the Service Worker API, conforming to
/// https://developer.mozilla.org/en-US/docs/Web/API/ServiceWorker
#[derive(Clone)]
#[napi]
pub struct ServiceWorker {
    pub(crate) script_url: String,
    pub(crate) scope: String,
    pub(crate) state: Arc<Mutex<ServiceWorkerState>>,
    pub(crate) handlers: SharedWorkerHandlers,
    pub(crate) inner_task: Arc<Mutex<Option<Task>>>,
}

#[napi]
impl ServiceWorker {
    /// Create a new ServiceWorker instance with `script_url` and optional configuration.
    #[napi(constructor)]
    pub fn new(env: Env, script_url: String, options: Option<ServiceWorkerOptions>) -> Result<Self> {
        let opts = options.unwrap_or_default();
        let scope = opts.scope.unwrap_or_else(|| "./".to_string());
        let handlers: SharedWorkerHandlers = Arc::new(Mutex::new(WorkerHandlers::new()));
        let state = Arc::new(Mutex::new(ServiceWorkerState::Installing));

        // Start background script task if valid
        let task_res = Task::new(
            env,
            TaskOptions {
                entries: Either::A(script_url.clone()),
                tasktree_path: None,
                sandbox: None,
                name: Some("ServiceWorker".to_string()),
                resource_limits: None,
                env: None,
                argv: None,
                exec_argv: None,
                task_data: None,
                track_unmanaged_fds: None,
                allow_network: Some(true),
                repository: None,
            },
        );

        let inner_task = Arc::new(Mutex::new(task_res.ok()));

        let sw = ServiceWorker {
            script_url,
            scope,
            state: Arc::clone(&state),
            handlers: Arc::clone(&handlers),
            inner_task,
        };

        // Transition through standard lifecycle asynchronously (installing -> installed -> activating -> activated)
        let state_clone = Arc::clone(&state);
        let handlers_clone = Arc::clone(&handlers);
        let (deferred, _promise) = env.create_deferred::<bool, DeferredBool>()?;
        deferred.resolve(Box::new(move |env: Env| {
            // Transition: installing -> installed
            {
                let mut s = state_clone.lock().unwrap();
                *s = ServiceWorkerState::Installed;
            }
            let _ = emit_statechange(&env, &handlers_clone, ServiceWorkerState::Installed.as_str());

            // Transition: installed -> activating
            {
                let mut s = state_clone.lock().unwrap();
                *s = ServiceWorkerState::Activating;
            }
            let _ = emit_statechange(&env, &handlers_clone, ServiceWorkerState::Activating.as_str());

            // Transition: activating -> activated
            {
                let mut s = state_clone.lock().unwrap();
                *s = ServiceWorkerState::Activated;
            }
            let _ = emit_statechange(&env, &handlers_clone, ServiceWorkerState::Activated.as_str());

            Ok(true)
        }));

        Ok(sw)
    }

    /// Serialized script URL defined for the ServiceWorker.
    #[napi(getter, js_name = "scriptURL")]
    pub fn script_url(&self) -> String {
        self.script_url.clone()
    }

    /// Scope URL for this service worker.
    #[napi(getter)]
    pub fn scope(&self) -> String {
        self.scope.clone()
    }

    /// Current lifecycle state of the service worker:
    /// 'parsed' | 'installing' | 'installed' | 'activating' | 'activated' | 'redundant'
    #[napi(getter)]
    pub fn state(&self) -> String {
        self.state.lock().unwrap().as_str().to_string()
    }

    /// Sends a message — consisting of any JavaScript object — to the service worker.
    #[napi]
    pub fn post_message(&self, env: Env, message: Unknown<'static>) -> Result<()> {
        if let Some(task) = self.inner_task.lock().unwrap().as_ref() {
            let _ = task.post_message(env, message);
        }
        Ok(())
    }

    // --- EventTarget interface ---------------------------------------------

    #[napi(getter)]
    pub fn get_onstatechange(
        &self,
        env: Env,
    ) -> Result<Option<Function<'static, Unknown<'static>, Unknown<'static>>>> {
        let g = self.handlers.lock().unwrap();
        match g.on_handlers.get("statechange").and_then(|v| v.as_ref()) {
            Some(h) => {
                let f = h.borrow_back(&env)?;
                let raw = f.raw();
                Ok(Some(unsafe { Function::from_napi_value(env.raw(), raw)? }))
            }
            None => Ok(None),
        }
    }

    #[napi(setter)]
    pub fn set_onstatechange(
        &self,
        _env: Env,
        handler: Option<Function<'static, Unknown<'static>, Unknown<'static>>>,
    ) -> Result<()> {
        let mut g = self.handlers.lock().unwrap();
        match handler {
            Some(f) => {
                let r = f.create_ref()?;
                g.on_handlers.insert("statechange".to_string(), Some(Arc::new(r)));
            }
            None => {
                g.on_handlers.remove("statechange");
            }
        }
        Ok(())
    }

    #[napi(getter)]
    pub fn get_onerror(
        &self,
        env: Env,
    ) -> Result<Option<Function<'static, Unknown<'static>, Unknown<'static>>>> {
        let g = self.handlers.lock().unwrap();
        match g.on_handlers.get("error").and_then(|v| v.as_ref()) {
            Some(h) => {
                let f = h.borrow_back(&env)?;
                let raw = f.raw();
                Ok(Some(unsafe { Function::from_napi_value(env.raw(), raw)? }))
            }
            None => Ok(None),
        }
    }

    #[napi(setter)]
    pub fn set_onerror(
        &self,
        _env: Env,
        handler: Option<Function<'static, Unknown<'static>, Unknown<'static>>>,
    ) -> Result<()> {
        let mut g = self.handlers.lock().unwrap();
        match handler {
            Some(f) => {
                let r = f.create_ref()?;
                g.on_handlers.insert("error".to_string(), Some(Arc::new(r)));
            }
            None => {
                g.on_handlers.remove("error");
            }
        }
        Ok(())
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
            .listeners
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
        let target = listener.raw();
        let mut g = self.handlers.lock().unwrap();
        let mut keep: Vec<SharedHandler> = Vec::new();
        for h in g.listeners.remove(&type_).unwrap_or_default() {
            let same = {
                let f: Function<Unknown<'static>, Unknown<'static>> = h.borrow_back(&env)?;
                let mut is_equal = false;
                unsafe {
                    napi::sys::napi_strict_equals(env.raw(), f.raw(), target, &mut is_equal);
                }
                is_equal
            };
            if !same {
                keep.push(h);
            }
        }
        g.listeners.insert(type_, keep);
        Ok(())
    }

    /// Dispatch an event to this worker's listeners (mirrors `EventTarget.dispatchEvent`).
    #[napi]
    pub fn dispatch_event(&self, env: Env, evt: Unknown<'static>) -> Result<()> {
        let evt_obj = evt.coerce_to_object()?;
        let type_: String = evt_obj.get_named_property("type")?;
        dispatch(&env, &self.handlers, &type_, &evt)
    }
}

/// The ServiceWorkerRegistration interface represents the service worker registration.
#[napi]
pub struct ServiceWorkerRegistration {
    scope: String,
    worker: ServiceWorker,
}

#[napi]
impl ServiceWorkerRegistration {
    #[napi(constructor)]
    pub fn new(env: Env, script_url: String, options: Option<ServiceWorkerOptions>) -> Result<Self> {
        let scope = options
            .as_ref()
            .and_then(|o| o.scope.clone())
            .unwrap_or_else(|| "./".to_string());
        let worker = ServiceWorker::new(env, script_url, options)?;
        Ok(ServiceWorkerRegistration { scope, worker })
    }

    #[napi(getter)]
    pub fn scope(&self) -> String {
        self.scope.clone()
    }

    /// Returns the active ServiceWorker instance if activating or activated, else None.
    #[napi(getter)]
    pub fn active(&self) -> Option<ServiceWorker> {
        let current = *self.worker.state.lock().unwrap();
        if current == ServiceWorkerState::Activating || current == ServiceWorkerState::Activated {
            Some(self.worker.clone())
        } else {
            None
        }
    }

    /// Returns the installing ServiceWorker instance if installing, else None.
    #[napi(getter)]
    pub fn installing(&self) -> Option<ServiceWorker> {
        let current = *self.worker.state.lock().unwrap();
        if current == ServiceWorkerState::Installing {
            Some(self.worker.clone())
        } else {
            None
        }
    }

    /// Returns the waiting ServiceWorker instance if installed, else None.
    #[napi(getter)]
    pub fn waiting(&self) -> Option<ServiceWorker> {
        let current = *self.worker.state.lock().unwrap();
        if current == ServiceWorkerState::Installed {
            Some(self.worker.clone())
        } else {
            None
        }
    }

    #[napi(ts_return_type = "Promise<boolean>")]
    pub fn unregister(&self, env: Env) -> Result<Unknown<'static>> {
        let (deferred, promise) = env.create_deferred::<bool, DeferredBool>()?;
        let state_clone = Arc::clone(&self.worker.state);
        let handlers_clone = Arc::clone(&self.worker.handlers);
        deferred.resolve(Box::new(move |env: Env| {
            {
                let mut s = state_clone.lock().unwrap();
                *s = ServiceWorkerState::Redundant;
            }
            let _ = emit_statechange(&env, &handlers_clone, "redundant");
            Ok(true)
        }));
        let raw = unsafe { ToNapiValue::to_napi_value(env.raw(), promise)? };
        Ok(unsafe { Unknown::from_raw_unchecked(env.raw(), raw) })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn dispatch(
    env: &Env,
    handlers: &SharedWorkerHandlers,
    type_: &str,
    event: &Unknown<'static>,
) -> Result<()> {
    let (on_handler, listeners) = {
        let g = handlers.lock().unwrap();
        let on = g.on_handlers.get(type_).and_then(|h| h.as_ref().cloned());
        let list = g.listeners.get(type_).cloned().unwrap_or_default();
        (on, list)
    };

    if let Some(h) = on_handler {
        let f: Function<Unknown<'static>, Unknown<'static>> = h.borrow_back(env)?;
        let _ = f.call(event.clone())?;
    }

    for h in listeners {
        let f: Function<Unknown<'static>, Unknown<'static>> = h.borrow_back(env)?;
        let _ = f.call(event.clone())?;
    }
    Ok(())
}

fn emit_statechange(env: &Env, handlers: &SharedWorkerHandlers, state: &str) -> Result<()> {
    let mut o = Object::new(env)?;
    o.set_named_property("type", "statechange")?;
    o.set_named_property("state", state)?;
    let evt = unsafe { Unknown::from_raw_unchecked(env.raw(), o.raw()) };
    dispatch(env, handlers, "statechange", &evt)
}
