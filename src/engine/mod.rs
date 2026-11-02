//! Engine module providing NAPI-RS bindings for FuseVM execution.
//!
//! Exposes `Engine` and `Program` classes, compiler frontend, host function registration,
//! output capture, and optional JIT execution.

pub mod ast;
pub mod compiler;
pub mod functions;
pub mod lexer;
pub mod parser;
pub mod symbols;
pub mod token;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use fusevm::{Chunk, VM, VMResult, Value as FuseValue};
use napi::bindgen_prelude::*;
use napi_derive::napi;
use serde_json::Value as JsonValue;

use crate::engine::compiler::{Compiler, EXT_HOST_CALL_BASE, EXT_PRINT};
use crate::engine::lexer::tokenize;
use crate::engine::parser::Parser;

/// Options for Engine construction.
#[napi(object)]
#[derive(Default, Clone, Debug)]
pub struct EngineOptions {
    /// Enable tracing JIT compilation if supported.
    pub jit: Option<bool>,
}

/// Execution result from running a Program.
#[napi(object)]
#[derive(Clone, Debug)]
pub struct RunResult {
    /// Resulting return value from execution.
    pub value: serde_json::Value,
    /// Captured standard output from `print()` statements.
    pub stdout: String,
}

#[derive(Clone, Copy)]
struct SendNapiEnv(usize);
unsafe impl Send for SendNapiEnv {}
unsafe impl Sync for SendNapiEnv {}

/// The Engine class that registers host functions and compiles Engine source into bytecode Programs.
#[napi(js_name = "Engine")]
pub struct Engine {
    jit_enabled: bool,
    host_functions: Arc<Mutex<HashMap<String, u16>>>,
    next_ext_id: Arc<Mutex<u16>>,
    callbacks: Arc<Mutex<HashMap<u16, FunctionRef<Vec<JsonValue>, JsonValue>>>>,
}

#[napi]
impl Engine {
    /// Constructs a new Engine instance.
    #[napi(constructor)]
    pub fn new(options: Option<EngineOptions>) -> Self {
        let jit_enabled = options.and_then(|o| o.jit).unwrap_or(false);
        Self {
            jit_enabled,
            host_functions: Arc::new(Mutex::new(HashMap::new())),
            next_ext_id: Arc::new(Mutex::new(EXT_HOST_CALL_BASE)),
            callbacks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a host JavaScript function callable from Engine source code.
    #[napi(ts_args_type = "name: string, callback: (...args: any[]) => any")]
    pub fn register_function(&self, name: String, callback: Function<Vec<JsonValue>, JsonValue>) -> Result<()> {
        let callback_ref = callback.create_ref()?;

        let mut next_id = self
            .next_ext_id
            .lock()
            .map_err(|e| Error::from_reason(format!("Lock error: {e}")))?;
        let id = *next_id;
        *next_id += 1;

        let mut hosts = self
            .host_functions
            .lock()
            .map_err(|e| Error::from_reason(format!("Lock error: {e}")))?;
        hosts.insert(name, id);

        let mut cbs = self
            .callbacks
            .lock()
            .map_err(|e| Error::from_reason(format!("Lock error: {e}")))?;
        cbs.insert(id, callback_ref);

        Ok(())
    }

    /// Compile Engine source code into an executable `Program`.
    #[napi]
    pub fn compile(&self, source: String, globals: Option<Vec<String>>) -> Result<Program> {
        let tokens = tokenize(&source).map_err(|e| Error::from_reason(format!("Lex error: {e}")))?;
        let mut parser = Parser::new(tokens);
        let statements = parser
            .parse()
            .map_err(|e| Error::from_reason(format!("Parse error: {e}")))?;

        let host_map = self
            .host_functions
            .lock()
            .map_err(|e| Error::from_reason(format!("Lock error: {e}")))?
            .clone();

        let known_globals = globals.unwrap_or_default();
        let compiler = Compiler::new(host_map, &known_globals);
        let compiled = compiler
            .compile(&statements)
            .map_err(|e| Error::from_reason(format!("Compile error: {e}")))?;

        let callbacks_clone = Arc::clone(&self.callbacks);

        Ok(Program {
            chunk: Arc::new(compiled.chunk),
            jit_enabled: self.jit_enabled,
            global_slots: compiled.global_slots,
            uses_host_functions: compiled.uses_host_functions,
            callbacks: callbacks_clone,
        })
    }
}

/// An immutable compiled Engine program ready for execution.
#[napi(js_name = "Program")]
pub struct Program {
    chunk: Arc<Chunk>,
    jit_enabled: bool,
    global_slots: HashMap<String, u16>,
    uses_host_functions: bool,
    callbacks: Arc<Mutex<HashMap<u16, FunctionRef<Vec<JsonValue>, JsonValue>>>>,
}

#[napi]
impl Program {
    /// Execute the program synchronously, returning the result value and captured stdout.
    #[napi]
    pub fn run(&self, env: Env, globals: Option<serde_json::Value>) -> Result<RunResult> {
        let chunk = (*self.chunk).clone();
        let mut vm = VM::new(chunk);

        if self.jit_enabled {
            vm.enable_tracing_jit();
        }

        // Apply provided globals to their matching slots
        if let Some(obj) = globals.as_ref().and_then(|g| g.as_object()) {
            for (k, v) in obj.iter() {
                if let Some(&slot) = self.global_slots.get(k) {
                    let fuse_val = json_to_fuse_value(v);
                    vm.set_slot(slot, fuse_val);
                }
            }
        }

        let captured_stdout = Arc::new(Mutex::new(String::new()));
        let stdout_clone = Arc::clone(&captured_stdout);
        let callbacks_clone = Arc::clone(&self.callbacks);
        let send_env = SendNapiEnv(env.raw() as usize);
        let _ = env;

        vm.set_extension_handler(Box::new(move |vm_ref: &mut VM, ext_id: u16, argc: u8| {
            if ext_id == EXT_PRINT {
                let val = vm_ref.pop();
                let mut out = stdout_clone.lock().expect("Stdout lock poisoned");
                let s = match val {
                    FuseValue::Int(i) => i.to_string(),
                    FuseValue::Float(f) => f.to_string(),
                    FuseValue::Bool(b) => b.to_string(),
                    FuseValue::Str(s) => s.as_str().to_string(),
                    FuseValue::Undef => "nil".to_string(),
                    _ => format!("{:?}", val),
                };
                out.push_str(&s);
                out.push('\n');
            } else {
                // Host function call synchronously on JS thread
                let mut args = Vec::new();
                for _ in 0..argc {
                    let v = vm_ref.pop();
                    args.push(fuse_value_to_json(&v));
                }
                args.reverse();

                let mut ret_val = FuseValue::Undef;

                let env_wrapper = Env::from_raw(send_env.0 as _);

                if let Ok(cbs) = callbacks_clone.lock() {
                    if let Some(cb_ref) = cbs.get(&ext_id) {
                        if let Ok(js_fn) = cb_ref.borrow_back(&env_wrapper) {
                            if let Ok(res) = js_fn.call(args) {
                                ret_val = json_to_fuse_value(&res);
                            }
                        }
                    }
                }

                vm_ref.push(ret_val);
            }
        }));

        match vm.run() {
            VMResult::Ok(val) => {
                let stdout = captured_stdout
                    .lock()
                    .map_err(|e| Error::from_reason(format!("Stdout lock error: {e}")))?
                    .clone();
                let json_val = fuse_value_to_json(&val);
                Ok(RunResult {
                    value: json_val,
                    stdout,
                })
            }
            VMResult::Error(err) => Err(Error::from_reason(format!("Runtime error: {err}"))),
            VMResult::Halted => {
                let stdout = captured_stdout
                    .lock()
                    .map_err(|e| Error::from_reason(format!("Stdout lock error: {e}")))?
                    .clone();
                Ok(RunResult {
                    value: JsonValue::Null,
                    stdout,
                })
            }
        }
    }

    /// Asynchronously execute the program.
    #[napi]
    pub async fn run_async(&self, globals: Option<serde_json::Value>) -> Result<RunResult> {
        if self.uses_host_functions {
            return Err(Error::from_reason(
                "Programs with host functions cannot be executed via runAsync()",
            ));
        }

        let chunk = Arc::clone(&self.chunk);
        let jit = self.jit_enabled;
        let global_slots = self.global_slots.clone();

        tokio::task::spawn_blocking(move || {
            let chunk_val = (*chunk).clone();
            let mut vm = VM::new(chunk_val);

            if jit {
                vm.enable_tracing_jit();
            }

            if let Some(obj) = globals.as_ref().and_then(|g| g.as_object()) {
                for (k, v) in obj.iter() {
                    if let Some(&slot) = global_slots.get(k) {
                        let fuse_val = json_to_fuse_value(v);
                        vm.set_slot(slot, fuse_val);
                    }
                }
            }

            let captured_stdout = Arc::new(Mutex::new(String::new()));
            let stdout_clone = Arc::clone(&captured_stdout);

            vm.set_extension_handler(Box::new(move |vm_ref: &mut VM, ext_id: u16, _argc: u8| {
                if ext_id == EXT_PRINT {
                    let val = vm_ref.pop();
                    let mut out = stdout_clone.lock().expect("Stdout lock poisoned");
                    let s = match val {
                        FuseValue::Int(i) => i.to_string(),
                        FuseValue::Float(f) => f.to_string(),
                        FuseValue::Bool(b) => b.to_string(),
                        FuseValue::Str(s) => s.as_str().to_string(),
                        FuseValue::Undef => "nil".to_string(),
                        _ => format!("{:?}", val),
                    };
                    out.push_str(&s);
                    out.push('\n');
                }
            }));

            match vm.run() {
                VMResult::Ok(val) => {
                    let stdout = captured_stdout.lock().unwrap().clone();
                    let json_val = fuse_value_to_json(&val);
                    Ok(RunResult {
                        value: json_val,
                        stdout,
                    })
                }
                VMResult::Error(err) => Err(Error::from_reason(format!("Runtime error: {err}"))),
                VMResult::Halted => {
                    let stdout = captured_stdout.lock().unwrap().clone();
                    Ok(RunResult {
                        value: JsonValue::Null,
                        stdout,
                    })
                }
            }
        })
        .await
        .map_err(|e| Error::from_reason(format!("Async join error: {e}")))?
    }
}

fn json_to_fuse_value(v: &JsonValue) -> FuseValue {
    match v {
        JsonValue::Null => FuseValue::Undef,
        JsonValue::Bool(b) => FuseValue::Bool(*b),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                FuseValue::Int(i)
            } else if let Some(f) = n.as_f64() {
                FuseValue::Float(f)
            } else {
                FuseValue::Undef
            }
        }
        JsonValue::String(s) => FuseValue::Str(Arc::new(s.clone())),
        _ => FuseValue::Undef,
    }
}

fn fuse_value_to_json(val: &FuseValue) -> JsonValue {
    match val {
        FuseValue::Undef => JsonValue::Null,
        FuseValue::Bool(b) => JsonValue::Bool(*b),
        FuseValue::Int(i) => JsonValue::Number((*i).into()),
        FuseValue::Float(f) => serde_json::Number::from_f64(*f)
            .map(JsonValue::Number)
            .unwrap_or(JsonValue::Null),
        FuseValue::Str(s) => JsonValue::String(s.as_str().to_string()),
        _ => JsonValue::String(format!("{:?}", val)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_engine_compilation_and_run() {
        let engine = Engine::new(None);
        let source = r#"
            let x = 10;
            let y = 20;
            let result = (x + y) * 2;
            print(result);
        "#;
        let prog = engine.compile(source.to_string(), None).unwrap();
        let chunk = (*prog.chunk).clone();
        let mut vm = VM::new(chunk);
        let mut out = String::new();
        vm.set_extension_handler(Box::new(move |vm_ref, ext_id, _| {
            if ext_id == EXT_PRINT {
                let v = vm_ref.pop();
                out.push_str(&format!("{:?}", v));
            }
        }));
        let _ = vm.run();
    }

    #[test]
    fn test_engine_while_loop() {
        let engine = Engine::new(None);
        let source = r#"
            let sum = 0;
            let i = 1;
            while (i <= 100) {
                sum = sum + i;
                i = i + 1;
            }
            print(sum);
        "#;
        let prog = engine.compile(source.to_string(), None).unwrap();
        let chunk = (*prog.chunk).clone();
        let mut vm = VM::new(chunk);
        let _ = vm.run();
    }

    #[test]
    fn test_engine_expression_value() {
        let engine = Engine::new(None);
        let prog = engine.compile("1 + 2;".to_string(), None).unwrap();
        let chunk = (*prog.chunk).clone();
        let mut vm = VM::new(chunk);
        let res = vm.run();
        match res {
            VMResult::Ok(v) => assert_eq!(v, FuseValue::Int(3)),
            _ => panic!("Expected Ok(Int(3))"),
        }
    }
}
