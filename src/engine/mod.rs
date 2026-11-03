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

#[cfg(test)]
mod compiler_tests;

use fusevm::{Chunk, VM, VMResult, Value};
use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::engine::async_run::RunAsyncTask;
use crate::engine::compiler::Compiler;
use crate::engine::errors::EngineError;
use crate::engine::host::{HostRegistry, HostRuntime};
use crate::engine::lexer::tokenize;
use crate::engine::parser::Parser;
use crate::engine::value::EngineValue;

fn create_engine_error(env: &Env, err: &EngineError) -> napi::Error {
    let mut js_err = napi::Error::new(Status::GenericFailure, err.message.clone());
    // Attach code, line, and column properties
    if let Ok(mut obj) = Object::new(env) {
        let _ = obj.set("code", err.code());
        let _ = obj.set("line", err.line as u32);
        let _ = obj.set("column", err.column as u32);
        let _ = obj.set("message", err.message.clone());
    }
    js_err
}

#[napi(object)]
#[derive(Default)]
pub struct EngineOptions {
    pub jit: Option<bool>,
}

#[napi(object)]
pub struct RunResult {
    pub value: Unknown<'static>,
    pub stdout: String,
}

#[napi]
pub struct Program {
    chunk: Chunk,
    jit: bool,
    uses_host_functions: bool,
    host_registry: Arc<Mutex<HostRegistry>>,
}

#[napi]
impl Program {
    #[napi(ts_args_type = "globals?: Record<string, any>")]
    pub fn run(&self, env: Env, globals: Option<Object>) -> Result<RunResult> {
        let mut vm = VM::new(self.chunk.clone());

        #[cfg(feature = "jit")]
        if self.jit {
            vm.enable_tracing_jit();
        }

        // Preload globals
        if let Some(globals_obj) = globals {
            let keys = Object::keys(&globals_obj)?;
            for key in keys {
                if let Some(pos) = vm.chunk.names.iter().position(|n| n == &key) {
                    let idx = pos as u16;
                    if let Some(js_val) = globals_obj.get::<Unknown>(&key)? {
                        let ev = EngineValue::from_js(&env, js_val)?;
                        let f_val = ev.to_fusevm();
                        if (idx as usize) >= vm.globals.len() {
                            vm.globals.resize(idx as usize + 1, Value::Undef);
                        }
                        vm.globals[idx as usize] = f_val;
                    }
                }
            }
        }

        let stdout = Arc::new(Mutex::new(String::new()));
        let fn_snapshot = {
            let host_lock = self.host_registry.lock().unwrap();
            host_lock.snapshot()
        };
        let mut runtime = HostRuntime::new(&env, fn_snapshot, Arc::clone(&stdout));
        runtime.install_handlers(&mut vm);

        let vm_res = vm.run();

        // Check if any error was raised by JS host functions
        if let Some(err) = runtime.take_error() {
            return Err(err);
        }

        let result_value = match vm_res {
            VMResult::Ok(val) => EngineValue::from_fusevm(&val),
            VMResult::Error(e) => {
                let mut err =
                    napi::Error::new(Status::GenericFailure, format!("Runtime error: {e}"));
                err.status = Status::GenericFailure;
                return Err(napi::Error::from_reason(format!("Runtime error: {e}")));
            }
            VMResult::Halted => EngineValue::Undef,
        };

        let js_value = result_value.to_js(&env)?;
        let stdout_str = stdout.lock().unwrap().clone();

        Ok(RunResult {
            value: unsafe { std::mem::transmute(js_value) },
            stdout: stdout_str,
        })
    }

    #[napi(
        ts_args_type = "globals?: Record<string, any>",
        ts_return_type = "Promise<RunResult>"
    )]
    pub fn run_async(
        &self,
        env: Env,
        globals: Option<Object>,
    ) -> Result<PromiseRaw<'static, Object<'static>>> {
        if self.uses_host_functions {
            return Err(napi::Error::new(
                Status::InvalidArg,
                "Cannot use runAsync on a program that uses host functions",
            ));
        }

        let mut parsed_globals = HashMap::new();
        if let Some(globals_obj) = globals {
            let keys = Object::keys(&globals_obj)?;
            for key in keys {
                if let Some(js_val) = globals_obj.get::<Unknown>(&key)? {
                    let ev = EngineValue::from_js(&env, js_val)?;
                    parsed_globals.insert(key, ev);
                }
            }
        }

        let task = RunAsyncTask {
            chunk: self.chunk.clone(),
            globals: parsed_globals,
            jit: self.jit,
        };

        let async_task = env.spawn(task)?;
        Ok(async_task.promise_object())
    }
}

#[napi]
pub struct Engine {
    jit: bool,
    host_registry: Arc<Mutex<HostRegistry>>,
}

#[napi]
impl Engine {
    #[napi(constructor)]
    pub fn new(options: Option<EngineOptions>) -> Result<Self> {
        let jit = options.and_then(|o| o.jit).unwrap_or(false);
        #[cfg(not(feature = "jit"))]
        if jit {
            return Err(napi::Error::new(
                Status::InvalidArg,
                "JIT execution is not supported on this platform/build configuration",
            ));
        }

        Ok(Self {
            jit,
            host_registry: Arc::new(Mutex::new(HostRegistry::new())),
        })
    }

    #[napi(ts_args_type = "name: string, jsFn: (...args: any[]) => any")]
    pub fn register_function(
        &mut self,
        name: String,
        js_fn: Function<Unknown<'static>, Unknown<'static>>,
    ) -> Result<()> {
        let mut registry = self.host_registry.lock().unwrap();
        registry.register(name, js_fn)
    }

    #[napi]
    pub fn compile(
        &self,
        env: Env,
        source: String,
        globals_keys: Option<Vec<String>>,
    ) -> Result<Program> {
        let tokens = tokenize(&source).map_err(|e| create_engine_error(&env, &e))?;

        let mut parser = Parser::new(tokens);
        let stmts = parser.parse().map_err(|e| create_engine_error(&env, &e))?;

        let host_lock = self.host_registry.lock().unwrap();
        let host_names: HashSet<String> = host_lock.names().into_iter().collect();
        let global_names = globals_keys.unwrap_or_default();

        let compiler = Compiler::new(host_names.clone(), global_names);
        let chunk = compiler
            .compile(&stmts)
            .map_err(|e| create_engine_error(&env, &e))?;

        // Determine if program uses any host functions
        let uses_host = chunk.names.iter().any(|n| host_names.contains(n));

        Ok(Program {
            chunk,
            jit: self.jit,
            uses_host_functions: uses_host,
            host_registry: Arc::clone(&self.host_registry),
        })
    }
}
