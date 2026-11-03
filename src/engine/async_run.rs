use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use napi::bindgen_prelude::{Task, Unknown};
use napi::{Env, Error, JsValue, Status};

use crate::engine::value::EngineValue;

pub struct RunTask {
    chunk: Arc<fusevm::Chunk>,
    supplied_globals: Vec<(u16, fusevm::Value)>,
    enable_jit: bool,
}

pub struct RunTaskOutput {
    pub value: EngineValue,
    pub stdout: String,
}

impl RunTask {
    pub fn new(
        chunk: Arc<fusevm::Chunk>,
        supplied_globals: Vec<(u16, fusevm::Value)>,
        enable_jit: bool,
    ) -> Self {
        Self {
            chunk,
            supplied_globals,
            enable_jit,
        }
    }
}

impl Task for RunTask {
    type Output = RunTaskOutput;
    type JsValue = crate::engine::RunResult;

    fn compute(&mut self) -> Result<Self::Output, Error> {
        let mut vm = fusevm::VM::new((*self.chunk).clone());

        #[cfg(feature = "jit")]
        if self.enable_jit {
            vm.enable_tracing_jit();
        }

        for (slot, val) in &self.supplied_globals {
            vm.set_slot(*slot, val.clone());
        }

        let stdout = Arc::new(Mutex::new(String::new()));
        let out_clone = Arc::clone(&stdout);
        vm.set_output_sink(Box::new(move |s| {
            out_clone.lock().unwrap().push_str(s);
        }));

        let res = vm.run();
        let value = match res {
            fusevm::VMResult::Ok(val) => EngineValue::from_fusevm(&val),
            fusevm::VMResult::Error(err) => {
                return Err(Error::new(Status::GenericFailure, format!("ENGINE_RUNTIME: {err}")));
            }
            fusevm::VMResult::Halted => EngineValue::Undef,
        };

        let stdout_str = std::mem::take(&mut *stdout.lock().unwrap());

        Ok(RunTaskOutput {
            value,
            stdout: stdout_str,
        })
    }

    fn resolve(&mut self, env: Env, output: Self::Output) -> Result<Self::JsValue, Error> {
        let js_val = output.value.to_js(&env)?;
        let static_val = unsafe { Unknown::from_raw_unchecked(env.raw(), js_val.raw()) };
        Ok(crate::engine::RunResult {
            value: static_val,
            stdout: output.stdout,
        })
    }
}
