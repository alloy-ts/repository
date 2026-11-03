//! Integration of fusevm for shell scripting and expression evaluation.

use std::collections::HashMap;
use fusevm::{ChunkBuilder, Op, Value, VM};

use crate::shell::vm::ShellExecutionHost;

pub struct ShellVMRunner {
    host: ShellExecutionHost,
}

impl ShellVMRunner {
    pub fn new(cwd: std::path::PathBuf, env: HashMap<String, String>) -> Self {
        Self {
            host: ShellExecutionHost::new(cwd, env),
        }
    }

    /// Evaluates brace expansions using fusevm VM and ShellHost
    pub fn eval_brace_expand(&mut self, s: &str) -> Vec<String> {
        let mut b = ChunkBuilder::new();
        let const_idx = b.add_constant(Value::str(s.to_string()));
        b.emit(Op::LoadConst(const_idx), 1);
        b.emit(Op::BraceExpand, 1);

        let mut vm = VM::new(b.build());
        vm.set_shell_host(Box::new(ShellExecutionHost::new(
            self.host.cwd.clone(),
            self.host.env.clone(),
        )));

        if let fusevm::VMResult::Ok(Value::Array(arr)) = vm.run() {
            arr.iter().map(|v| v.to_str()).collect()
        } else {
            vec![s.to_string()]
        }
    }
}
