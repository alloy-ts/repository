use fusevm::{VM, Value};
use napi::bindgen_prelude::*;
use napi::{Env, check_status, sys};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::engine::compiler::EXT_HOST_CALL_BASE;
use crate::engine::value::EngineValue;

pub struct HostRegistry {
    functions: HashMap<String, Arc<FunctionRef<Unknown<'static>, Unknown<'static>>>>,
}

impl HostRegistry {
    pub fn new() -> Self {
        Self {
            functions: HashMap::new(),
        }
    }

    pub fn register(
        &mut self,
        name: String,
        js_fn: Function<Unknown<'static>, Unknown<'static>>,
    ) -> Result<()> {
        let fn_ref = js_fn.create_ref()?;
        self.functions.insert(name, Arc::new(fn_ref));
        Ok(())
    }

    pub fn names(&self) -> Vec<String> {
        self.functions.keys().cloned().collect()
    }

    pub fn has_function(&self, name: &str) -> bool {
        self.functions.contains_key(name)
    }

    pub fn is_empty(&self) -> bool {
        self.functions.is_empty()
    }

    pub fn snapshot(
        &self,
    ) -> HashMap<String, Arc<FunctionRef<Unknown<'static>, Unknown<'static>>>> {
        self.functions.clone()
    }
}

pub struct HostRuntime<'a> {
    env: &'a Env,
    functions: HashMap<String, Arc<FunctionRef<Unknown<'static>, Unknown<'static>>>>,
    stdout: Arc<Mutex<String>>,
    error: Arc<Mutex<Option<napi::Error>>>,
}

impl<'a> HostRuntime<'a> {
    pub fn new(
        env: &'a Env,
        functions: HashMap<String, Arc<FunctionRef<Unknown<'static>, Unknown<'static>>>>,
        stdout: Arc<Mutex<String>>,
    ) -> Self {
        Self {
            env,
            functions,
            stdout,
            error: Arc::new(Mutex::new(None)),
        }
    }

    pub fn install_handlers(&mut self, vm: &mut VM) {
        // Output sink for print statements
        let stdout_sink = Arc::clone(&self.stdout);
        vm.set_output_sink(Box::new(move |s| {
            if let Ok(mut lock) = stdout_sink.lock() {
                lock.push_str(s);
            }
        }));

        let env_ptr = self.env as *const Env as usize;
        let fn_map_ptr = &self.functions
            as *const HashMap<String, Arc<FunctionRef<Unknown<'static>, Unknown<'static>>>>
            as usize;
        let err_clone = Arc::clone(&self.error);

        vm.set_extension_handler(Box::new(move |vm, id, argc| {
            if err_clone.lock().unwrap().is_some() {
                vm.request_halt();
                return;
            }

            if id >= EXT_HOST_CALL_BASE {
                let name_idx = (id - EXT_HOST_CALL_BASE) as usize;
                let fn_name = match vm.chunk.names.get(name_idx) {
                    Some(n) => n.clone(),
                    None => {
                        vm.push(Value::Undef);
                        return;
                    }
                };

                // Pop argc values from VM stack in reverse order
                let mut raw_args = Vec::with_capacity(argc as usize);
                for _ in 0..argc {
                    raw_args.push(vm.pop());
                }
                raw_args.reverse();

                let env = unsafe { &*(env_ptr as *const Env) };
                let fn_map = unsafe {
                    &*(fn_map_ptr
                        as *const HashMap<
                            String,
                            Arc<FunctionRef<Unknown<'static>, Unknown<'static>>>,
                        >)
                };

                let fn_ref = match fn_map.get(&fn_name) {
                    Some(r) => r,
                    None => {
                        vm.push(Value::Undef);
                        return;
                    }
                };

                let js_fn = match fn_ref.borrow_back(env) {
                    Ok(f) => f,
                    Err(e) => {
                        *err_clone.lock().unwrap() = Some(e);
                        vm.push(Value::Undef);
                        vm.request_halt();
                        return;
                    }
                };

                // Convert arguments to JS values
                let mut js_args: Vec<sys::napi_value> = Vec::with_capacity(raw_args.len());
                for arg in raw_args {
                    let ev = EngineValue::from_fusevm(&arg);
                    match ev.to_js(env) {
                        Ok(jval) => js_args.push(jval.raw()),
                        Err(e) => {
                            *err_clone.lock().unwrap() = Some(e);
                            vm.push(Value::Undef);
                            vm.request_halt();
                            return;
                        }
                    }
                }

                let mut raw_this = std::ptr::null_mut();
                if let Err(e) = check_status!(
                    unsafe { sys::napi_get_undefined(env.raw(), &mut raw_this) },
                    "Get undefined value failed"
                ) {
                    *err_clone.lock().unwrap() = Some(e);
                    vm.push(Value::Undef);
                    vm.request_halt();
                    return;
                }

                let mut raw_return = std::ptr::null_mut();
                let status = unsafe {
                    sys::napi_call_function(
                        env.raw(),
                        raw_this,
                        js_fn.raw(),
                        js_args.len(),
                        js_args.as_ptr(),
                        &mut raw_return,
                    )
                };

                if status != sys::Status::napi_ok {
                    let mut exception = std::ptr::null_mut();
                    unsafe { sys::napi_get_and_clear_last_exception(env.raw(), &mut exception) };
                    let err_msg = if !exception.is_null() {
                        let unknown_exc = unsafe { Unknown::from_napi_value(env.raw(), exception) };
                        if let Ok(u) = unknown_exc {
                            if let Ok(s) = u.coerce_to_string() {
                                if let Ok(utf8) = s.into_utf8() {
                                    utf8.into_owned().unwrap_or_else(|_| {
                                        "Host function threw an exception".to_string()
                                    })
                                } else {
                                    "Host function threw an exception".to_string()
                                }
                            } else {
                                "Host function threw an exception".to_string()
                            }
                        } else {
                            "Host function threw an exception".to_string()
                        }
                    } else {
                        "Host function threw an exception".to_string()
                    };
                    *err_clone.lock().unwrap() =
                        Some(napi::Error::new(Status::GenericFailure, err_msg));
                    vm.push(Value::Undef);
                    vm.request_halt();
                    return;
                }

                let unknown = unsafe { Unknown::from_napi_value(env.raw(), raw_return) };
                match unknown.and_then(|u| EngineValue::from_js(env, u)) {
                    Ok(ev) => {
                        vm.push(ev.to_fusevm());
                    }
                    Err(e) => {
                        *err_clone.lock().unwrap() = Some(e);
                        vm.push(Value::Undef);
                        vm.request_halt();
                    }
                }
            }
        }));
    }

    pub fn take_error(&mut self) -> Option<napi::Error> {
        self.error.lock().unwrap().take()
    }
}
