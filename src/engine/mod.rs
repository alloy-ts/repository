pub mod ast;
pub mod async_run;
pub mod compiler;
pub mod errors;
pub mod functions;
pub mod host;
pub mod lexer;
pub mod parser;
pub mod symbols;
pub mod token;
pub mod value;

use std::collections::HashMap;
use std::sync::Arc;

use napi::bindgen_prelude::{
    AsyncTask, FromNapiValue, Function, FunctionRef, JsError, JsObjectValue, ToNapiValue, Unknown,
};
use napi::{sys, Env, JsValue, Status};
use napi_derive::napi;

use crate::engine::async_run::RunTask;
use crate::engine::compiler::{compile, CompiledProgram};
use crate::engine::errors::EngineError;
use crate::engine::host::{HostRegistry, Session, SharedHostFunction};
use crate::engine::parser::parse;
use crate::engine::value::EngineValue;

fn to_napi_engine_error(err: EngineError, env: &Env) -> napi::Error {
    let js_err = JsError::from(napi::Error::new(Status::GenericFailure, err.to_string()));
    let raw_err = unsafe { js_err.into_value(env.raw()) };
    if raw_err.is_null() {
        return napi::Error::new(Status::GenericFailure, err.to_string());
    }

    unsafe {
        let code_key = std::ffi::CString::new("code").unwrap();
        let code_val = ToNapiValue::to_napi_value(env.raw(), err.kind.code()).unwrap_or(std::ptr::null_mut());
        let _ = sys::napi_set_named_property(env.raw(), raw_err, code_key.as_ptr(), code_val);

        let msg_key = std::ffi::CString::new("message").unwrap();
        let msg_val = ToNapiValue::to_napi_value(env.raw(), err.message.as_str()).unwrap_or(std::ptr::null_mut());
        let _ = sys::napi_set_named_property(env.raw(), raw_err, msg_key.as_ptr(), msg_val);

        let line_key = std::ffi::CString::new("line").unwrap();
        let line_val = ToNapiValue::to_napi_value(env.raw(), err.line as i64).unwrap_or(std::ptr::null_mut());
        let _ = sys::napi_set_named_property(env.raw(), raw_err, line_key.as_ptr(), line_val);

        let col_key = std::ffi::CString::new("column").unwrap();
        let col_val = ToNapiValue::to_napi_value(env.raw(), err.column as i64).unwrap_or(std::ptr::null_mut());
        let _ = sys::napi_set_named_property(env.raw(), raw_err, col_key.as_ptr(), col_val);
    }

    let unknown = unsafe { Unknown::from_raw_unchecked(env.raw(), raw_err) };
    unknown.into()
}

#[napi(object)]
pub struct EngineOptions {
    pub jit: Option<bool>,
    pub max_steps: Option<u32>,
}

#[napi(object)]
pub struct RunResult {
    pub value: Unknown<'static>,
    pub stdout: String,
}

#[napi]
pub struct Engine {
    jit: bool,
    max_steps: Option<u32>,
    host_registry: HostRegistry,
}

#[napi]
impl Engine {
    #[napi(constructor)]
    pub fn new(options: Option<EngineOptions>) -> napi::Result<Self> {
        let (jit, max_steps) = match options {
            Some(opts) => {
                let j = opts.jit.unwrap_or(false);
                #[cfg(not(feature = "jit"))]
                if j {
                    return Err(napi::Error::new(
                        Status::GenericFailure,
                        "ENGINE_JIT_UNAVAILABLE: JIT feature is not enabled",
                    ));
                }
                (j, opts.max_steps)
            }
            None => (false, None),
        };

        Ok(Self {
            jit,
            max_steps,
            host_registry: HostRegistry::new(),
        })
    }

    #[napi]
    pub fn register_function(
        &mut self,
        env: Env,
        name: String,
        func: Function<'static, Unknown<'static>, Unknown<'static>>,
    ) -> napi::Result<()> {
        let func_ref = func.create_ref()?;
        self.host_registry
            .register(&name, Arc::new(func_ref))
            .map_err(|e| to_napi_engine_error(e, &env))?;

        Ok(())
    }

    #[napi]
    pub fn compile(&self, env: Env, source: String) -> napi::Result<Program> {
        let ast = parse(&source).map_err(|e| to_napi_engine_error(e, &env))?;
        let host_map = self.host_registry.get_id_map();
        let compiled = compile(&ast, &host_map).map_err(|e| to_napi_engine_error(e, &env))?;

        Ok(Program::create(
            compiled,
            self.jit,
            &self.host_registry,
        ))
    }
}

#[napi]
pub struct Program {
    compiled: CompiledProgram,
    jit: bool,
    host_registry: HostRegistry,
}

impl Program {
    pub fn create(
        compiled: CompiledProgram,
        jit: bool,
        registry: &HostRegistry,
    ) -> Self {
        Self {
            compiled,
            jit,
            host_registry: registry.clone(),
        }
    }

    fn extract_globals(
        &self,
        env: &Env,
        globals: Option<Unknown>,
    ) -> napi::Result<Vec<(u16, fusevm::Value)>> {
        let mut supplied = Vec::new();
        if let Some(g) = globals {
            if g.get_type()? != napi::ValueType::Undefined && g.get_type()? != napi::ValueType::Null {
                let obj = g.coerce_to_object()?;
                let prop_names = obj.get_property_names()?;
                let len = prop_names.get_array_length()?;

                for i in 0..len {
                    let key_val: Unknown = prop_names.get_element(i)?;
                    let key = key_val
                        .coerce_to_string()?
                        .into_utf8()?
                        .into_owned()?;

                    if let Some(&slot) = self.compiled.globals.get(&key) {
                        let prop_val: Unknown = obj.get_named_property(&key)?;
                        let ev = EngineValue::from_js(env, prop_val)
                            .map_err(|e| to_napi_engine_error(e, env))?;
                        supplied.push((slot, ev.to_fusevm()));
                    } else {
                        return Err(to_napi_engine_error(
                            EngineError::unknown_global(format!("Unknown global variable '{key}'")),
                            env,
                        ));
                    }
                }
            }
        }
        Ok(supplied)
    }
}

#[napi]
impl Program {
    #[napi(getter)]
    pub fn uses_host_functions(&self) -> bool {
        self.compiled.uses_host_functions
    }

    #[napi]
    pub fn run(
        &self,
        env: Env,
        globals: Option<Unknown>,
    ) -> napi::Result<RunResult> {
        let supplied_globals = self.extract_globals(&env, globals)?;

        let mut vm = fusevm::VM::new((*self.compiled.chunk).clone());

        #[cfg(feature = "jit")]
        if self.jit {
            vm.enable_tracing_jit();
        }

        for (slot, val) in supplied_globals {
            vm.set_slot(slot, val);
        }

        let session = Session::new(&env, &self.host_registry);
        session.install(&mut vm);

        let vm_res = vm.run();
        session.check_error()?;

        let stdout = session.take_stdout();

        let value = match vm_res {
            fusevm::VMResult::Ok(val) => EngineValue::from_fusevm(&val),
            fusevm::VMResult::Error(err) => {
                return Err(to_napi_engine_error(EngineError::runtime(err), &env));
            }
            fusevm::VMResult::Halted => EngineValue::Undef,
        };

        let js_value = value.to_js(&env)?;
        let static_val = unsafe { Unknown::from_raw_unchecked(env.raw(), js_value.raw()) };

        Ok(RunResult {
            value: static_val,
            stdout,
        })
    }

    #[napi]
    pub fn run_async(
        &self,
        env: Env,
        globals: Option<Unknown>,
    ) -> napi::Result<AsyncTask<RunTask>> {
        if self.compiled.uses_host_functions {
            return Err(to_napi_engine_error(
                EngineError::async_host("Cannot run programs with host functions asynchronously"),
                &env,
            ));
        }

        let supplied_globals = self.extract_globals(&env, globals)?;

        let task = RunTask::new(
            Arc::clone(&self.compiled.chunk),
            supplied_globals,
            self.jit,
        );

        Ok(AsyncTask::new(task))
    }
}
