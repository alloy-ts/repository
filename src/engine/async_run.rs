use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use fusevm::{Chunk, VM, VMResult, Value};
use napi::bindgen_prelude::*;
use napi::Env;

use crate::engine::value::EngineValue;

pub struct RunAsyncTask {
    pub chunk: Chunk,
    pub globals: HashMap<String, EngineValue>,
    pub jit: bool,
}

pub struct AsyncOutput {
    pub value: EngineValue,
    pub stdout: String,
}

impl Task for RunAsyncTask {
    type Output = AsyncOutput;
    type JsValue = Object<'static>;

    fn compute(&mut self) -> Result<Self::Output> {
        let mut vm = VM::new(self.chunk.clone());

        #[cfg(feature = "jit")]
        if self.jit {
            vm.enable_tracing_jit();
        }

        // Set up globals
        for (name, val) in &self.globals {
            if let Some(pos) = vm.chunk.names.iter().position(|n| n == name) {
                let idx = pos as u16;
                if (idx as usize) >= vm.globals.len() {
                    vm.globals.resize(idx as usize + 1, Value::Undef);
                }
                vm.globals[idx as usize] = val.to_fusevm();
            }
        }

        let stdout = Arc::new(Mutex::new(String::new()));
        let out_clone = Arc::clone(&stdout);
        vm.set_output_sink(Box::new(move |s| {
            if let Ok(mut lock) = out_clone.lock() {
                lock.push_str(s);
            }
        }));

        let res = vm.run();
        let value = match res {
            VMResult::Ok(val) => EngineValue::from_fusevm(&val),
            VMResult::Error(err) => {
                return Err(napi::Error::new(
                    Status::GenericFailure,
                    format!("Runtime error: {err}"),
                ));
            }
            VMResult::Halted => EngineValue::Undef,
        };

        let stdout_str = stdout.lock().unwrap().clone();
        Ok(AsyncOutput {
            value,
            stdout: stdout_str,
        })
    }

    fn resolve(&mut self, env: Env, output: Self::Output) -> Result<Self::JsValue> {
        let mut obj = Object::new(&env)?;
        let js_val = output.value.to_js(&env)?;
        let js_stdout = ToNapiValue::into_unknown(output.stdout, &env)?;
        obj.set("value", js_val)?;
        obj.set("stdout", js_stdout)?;
        Ok(unsafe { std::mem::transmute(obj) })
    }
}
