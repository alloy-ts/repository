pub mod ast;
pub mod compiler;
pub mod functions;
pub mod lexer;
pub mod parser;
pub mod symbols;
pub mod token;
pub mod value;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use fusevm::{Chunk, VMResult, VM};
use napi::bindgen_prelude::*;
use napi_derive::napi;

use crate::engine::compiler::Compiler;
use crate::engine::parser::parse;
use crate::engine::value::EngineValue;

#[napi(object)]
pub struct EngineOptions {
    pub jit: Option<bool>,
}

#[napi(object)]
pub struct RunResult {
    pub value: Option<String>,
    pub stdout: String,
}

#[napi]
pub struct Program {
    chunk: Arc<Chunk>,
    jit: bool,
}

#[napi]
impl Program {
    #[napi]
    pub fn run(&self) -> Result<RunResult> {
        let mut vm = VM::new((*self.chunk).clone());

        if self.jit {
            vm.enable_tracing_jit();
        }

        let stdout_buf = Arc::new(Mutex::new(String::new()));
        let sink_buf = Arc::clone(&stdout_buf);
        vm.set_output_sink(Box::new(move |s| {
            if let Ok(mut lock) = sink_buf.lock() {
                lock.push_str(s);
            }
        }));

        let vm_res = vm.run();
        let stdout = stdout_buf.lock().map(|s| s.clone()).unwrap_or_default();

        match vm_res {
            VMResult::Ok(val) => {
                let eng_val = EngineValue::from_fusevm(&val);
                Ok(RunResult {
                    value: Some(eng_val.to_string()),
                    stdout,
                })
            }
            VMResult::Halted => Ok(RunResult {
                value: None,
                stdout,
            }),
            VMResult::Error(err) => Err(Error::new(Status::GenericFailure, err)),
        }
    }
}

#[napi]
pub struct Engine {
    jit: bool,
    host_functions: HashMap<String, u16>,
}

#[napi]
impl Engine {
    #[napi(constructor)]
    pub fn new(options: Option<EngineOptions>) -> Self {
        let jit = options.and_then(|o| o.jit).unwrap_or(false);
        Self {
            jit,
            host_functions: HashMap::new(),
        }
    }

    #[napi]
    pub fn compile(&self, source: String) -> Result<Program> {
        let statements = parse(&source).map_err(|e| Error::new(Status::InvalidArg, e))?;
        let compiler = Compiler::with_host_functions(self.host_functions.clone());
        let chunk_builder = compiler
            .compile(&statements)
            .map_err(|e| Error::new(Status::GenericFailure, e))?;
        let chunk = chunk_builder.build();

        Ok(Program {
            chunk: Arc::new(chunk),
            jit: self.jit,
        })
    }

    #[napi]
    pub fn execute(&self, source: String) -> Result<RunResult> {
        let program = self.compile(source)?;
        program.run()
    }
}

#[cfg(test)]
mod tests {
    use super::compiler::Compiler;
    use super::parser::parse;
    use fusevm::{VM, VMResult};

    #[test]
    fn test_engine_arithmetic() {
        let code = "let x = 10; let y = 20; let z = (x + y) * 2; print(z);";
        let stmts = parse(code).unwrap();
        let chunk = Compiler::new().compile(&stmts).unwrap().build();
        let mut vm = VM::new(chunk);
        let res = vm.run();
        assert!(matches!(res, VMResult::Ok(_) | VMResult::Halted));
    }

    #[test]
    fn test_engine_while_loop() {
        let code = r#"
            let sum = 0;
            let i = 1;
            while (i <= 100) {
                sum = sum + i;
                i = i + 1;
            }
            print(sum);
        "#;
        let stmts = parse(code).unwrap();
        let chunk = Compiler::new().compile(&stmts).unwrap().build();
        let mut vm = VM::new(chunk);
        let res = vm.run();
        assert!(matches!(res, VMResult::Ok(_) | VMResult::Halted));
    }

    #[test]
    fn test_engine_function_recursion() {
        let code = r#"
            fn fact(n) {
                if (n <= 1) {
                    return 1;
                }
                return n * fact(n - 1);
            }
            let res = fact(5);
            print(res);
        "#;
        let stmts = parse(code).unwrap();
        let chunk = Compiler::new().compile(&stmts).unwrap().build();
        let mut vm = VM::new(chunk);
        let res = vm.run();
        assert!(matches!(res, VMResult::Ok(_) | VMResult::Halted));
    }
}
