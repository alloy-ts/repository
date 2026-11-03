use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use napi::bindgen_prelude::{Function, FunctionRef, Unknown};
use napi::{sys, Env, JsValue};

use crate::engine::compiler::EXT_HOST_BASE;
use crate::engine::errors::EngineError;
use crate::engine::value::EngineValue;

pub type SharedHostFunction = Arc<FunctionRef<Unknown<'static>, Unknown<'static>>>;

#[derive(Clone)]
pub struct HostRegistry {
    pub(crate) functions: HashMap<String, (u16, SharedHostFunction)>,
    next_id: u16,
}

impl HostRegistry {
    pub fn new() -> Self {
        Self {
            functions: HashMap::new(),
            next_id: 0,
        }
    }

    pub fn register(
        &mut self,
        name: &str,
        func: SharedHostFunction,
    ) -> Result<u16, EngineError> {
        if self.functions.contains_key(name) {
            return Err(EngineError::duplicate_host(format!(
                "Host function '{name}' already registered"
            )));
        }

        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| EngineError::range("Too many host functions"))?;

        self.functions.insert(name.to_string(), (id, func));
        Ok(id)
    }

    pub fn get_id_map(&self) -> HashMap<String, u16> {
        self.functions
            .iter()
            .map(|(k, (id, _))| (k.clone(), *id))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.functions.is_empty()
    }
}

pub struct Session<'a> {
    env: &'a Env,
    registry: &'a HostRegistry,
    stdout: Arc<Mutex<String>>,
    pending_error: Arc<Mutex<Option<napi::Error>>>,
}

impl<'a> Session<'a> {
    pub fn new(env: &'a Env, registry: &'a HostRegistry) -> Self {
        Self {
            env,
            registry,
            stdout: Arc::new(Mutex::new(String::new())),
            pending_error: Arc::new(Mutex::new(None)),
        }
    }

    pub fn install(&self, vm: &mut fusevm::VM) {
        // Output sink for print()
        let out_buf = Arc::clone(&self.stdout);
        vm.set_output_sink(Box::new(move |s| {
            out_buf.lock().unwrap().push_str(s);
        }));

        // Extension handler for host function calls
        let env_ptr = self.env as *const Env as usize;
        let id_to_func: HashMap<u16, SharedHostFunction> = self
            .registry
            .functions
            .values()
            .map(|(id, f)| (*id, Arc::clone(f)))
            .collect();

        let pending_err_clone = Arc::clone(&self.pending_error);

        vm.set_extension_handler(Box::new(move |vm, id, argc| {
            if id >= EXT_HOST_BASE {
                let host_id = id - EXT_HOST_BASE;
                if let Some(func_ref) = id_to_func.get(&host_id) {
                    let mut args = Vec::with_capacity(argc as usize);
                    for _ in 0..argc {
                        let v = vm.pop();
                        args.push(EngineValue::from_fusevm(&v));
                    }
                    args.reverse();

                    let env = unsafe { &*(env_ptr as *const Env) };

                    let res: Result<fusevm::Value, napi::Error> = (|| {
                        let js_func: Function<Unknown<'static>, Unknown<'static>> =
                            func_ref.borrow_back(env)?;
                        let mut raw_args = Vec::with_capacity(argc as usize);
                        for arg in &args {
                            let js_val = arg.to_js(env)?;
                            raw_args.push(js_val.raw());
                        }

                        let mut raw_this = ptr::null_mut();
                        unsafe {
                            sys::napi_get_undefined(env.raw(), &mut raw_this);
                        }

                        let mut raw_return = ptr::null_mut();
                        let status = unsafe {
                            sys::napi_call_function(
                                env.raw(),
                                raw_this,
                                js_func.raw(),
                                raw_args.len(),
                                raw_args.as_ptr(),
                                &mut raw_return,
                            )
                        };

                        if status != sys::Status::napi_ok {
                            return Err(napi::Error::from_status(napi::Status::from(status)));
                        }

                        let ret_unknown = unsafe { Unknown::from_raw_unchecked(env.raw(), raw_return) };
                        let ev = EngineValue::from_js(env, ret_unknown)
                            .map_err(|e| napi::Error::new(napi::Status::GenericFailure, e.message))?;
                        Ok(ev.to_fusevm())
                    })();

                    match res {
                        Ok(ret_val) => {
                            vm.push(ret_val);
                        }
                        Err(e) => {
                            *pending_err_clone.lock().unwrap() = Some(e);
                            vm.push(fusevm::Value::Undef);
                            vm.request_halt();
                        }
                    }
                }
            }
        }));
    }

    pub fn take_stdout(&self) -> String {
        std::mem::take(&mut *self.stdout.lock().unwrap())
    }

    pub fn check_error(&self) -> Result<(), napi::Error> {
        if let Some(err) = self.pending_error.lock().unwrap().take() {
            Err(err)
        } else {
            Ok(())
        }
    }
}
