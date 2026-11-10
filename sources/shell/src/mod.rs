//! Bun-compatible shell runner using `fusevm` and native async process pipelines.

pub mod ast;
pub mod builtins;
pub mod child_process;
pub mod fusevm_bridge;
pub mod parser;
pub mod repo_builtin;
pub mod vm;

pub use child_process::{
    ChildProcessOptions, ExecResult, exec, exec_file, exec_file_sync, exec_sync, spawn, spawn_sync,
};

use napi::bindgen_prelude::{Buffer, Result, Status};
use napi_derive::napi;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::shell::parser::parse_shell_script;
use crate::shell::vm::execute_pipeline_sequence;

#[napi(object)]
#[derive(Clone, Default)]
pub struct ShellConfig {
    pub cwd: Option<String>,
    pub env: Option<HashMap<String, String>>,
    pub nothrow: Option<bool>,
}

#[napi]
pub struct Shell {
    config: Arc<Mutex<ShellConfig>>,
}

#[napi]
impl Shell {
    #[napi(constructor)]
    pub fn new(config: Option<ShellConfig>) -> Self {
        Self {
            config: Arc::new(Mutex::new(config.unwrap_or_default())),
        }
    }

    #[napi]
    pub fn cwd(&self, path: Option<String>) -> Option<String> {
        let mut cfg = self.config.lock().unwrap();
        if let Some(p) = path {
            cfg.cwd = Some(p.clone());
            Some(p)
        } else {
            cfg.cwd.clone()
        }
    }

    #[napi]
    pub fn env(
        &self,
        env_vars: Option<HashMap<String, String>>,
    ) -> Option<HashMap<String, String>> {
        let mut cfg = self.config.lock().unwrap();
        if let Some(ev) = env_vars {
            cfg.env = Some(ev.clone());
            Some(ev)
        } else {
            cfg.env.clone()
        }
    }

    #[napi]
    pub fn nothrow(&self) {
        let mut cfg = self.config.lock().unwrap();
        cfg.nothrow = Some(true);
    }

    #[napi]
    pub fn throws(&self, throw_on_error: bool) {
        let mut cfg = self.config.lock().unwrap();
        cfg.nothrow = Some(!throw_on_error);
    }

    #[napi]
    pub async fn run(
        &self,
        script: String,
        local_cfg: Option<ShellConfig>,
    ) -> Result<ShellPromiseResult> {
        let (cwd, env_map, nothrow) = {
            let base = self.config.lock().unwrap();
            let cwd = local_cfg
                .as_ref()
                .and_then(|c| c.cwd.clone())
                .or_else(|| base.cwd.clone())
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

            let mut env_map = base
                .env
                .clone()
                .unwrap_or_else(|| std::env::vars().collect());
            if let Some(loc) = local_cfg.as_ref().and_then(|c| c.env.clone()) {
                for (k, v) in loc {
                    env_map.insert(k, v);
                }
            }

            let nothrow = local_cfg
                .as_ref()
                .and_then(|c| c.nothrow)
                .or(base.nothrow)
                .unwrap_or(false);

            (cwd, env_map, nothrow)
        };

        let seq =
            parse_shell_script(&script).map_err(|e| napi::Error::new(Status::GenericFailure, e))?;

        let output = execute_pipeline_sequence(&seq, &cwd, &env_map).await;

        if output.exit_code != 0 && !nothrow {
            let stderr_str = String::from_utf8_lossy(&output.stderr);
            let err = napi::Error::new(
                Status::GenericFailure,
                format!(
                    "Shell command failed with exit code {}: {}",
                    output.exit_code, stderr_str
                ),
            );
            return Err(err);
        }

        Ok(ShellPromiseResult {
            stdout: Buffer::from(output.stdout),
            stderr: Buffer::from(output.stderr),
            exit_code: output.exit_code,
        })
    }
}

#[napi(object)]
pub struct ShellPromiseResult {
    pub stdout: Buffer,
    pub stderr: Buffer,
    pub exit_code: i32,
}

#[napi]
pub fn braces(pattern: String) -> Vec<String> {
    let mut bridge = fusevm_bridge::ShellVMRunner::new(
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        std::env::vars().collect(),
    );
    bridge.eval_brace_expand(&pattern)
}

#[napi]
pub fn escape(s: String) -> String {
    let mut escaped = String::new();
    for c in s.chars() {
        match c {
            '$' | '`' | '"' | '\\' => {
                escaped.push('\\');
                escaped.push(c);
            }
            _ => escaped.push(c),
        }
    }
    escaped
}
