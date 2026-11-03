//! `repo` builtin implementation hooking into `src/repo/mod.rs` (gix-backed repository).

use std::path::Path;
use crate::repo::init::InitOptions;
use crate::repo::Repository;
use crate::shell::builtins::BuiltinResult;

pub fn builtin_repo(args: &[String], cwd: &Path) -> BuiltinResult {
    if args.is_empty() {
        let help = b"Usage: repo <subcommand> [args...]\n\
Subcommands:\n  \
status           Show status of repository at cwd\n  \
init [path]      Initialize a new repository\n  \
head             Show current HEAD commit ID\n  \
is-repo [path]   Check if path is a git repository\n  \
is-bare          Check if repository is bare\n";
        return BuiltinResult {
            exit_code: 0,
            stdout: help.to_vec(),
            stderr: Vec::new(),
        };
    }

    let subcmd = &args[0];
    let subargs = &args[1..];

    match subcmd.as_str() {
        "status" => {
            let path_str = cwd.to_string_lossy().to_string();
            match Repository::open(path_str) {
                Ok(repo) => {
                    let mut out = String::new();
                    out.push_str(&format!("git_dir: {}\n", repo.git_dir()));
                    if let Some(wd) = repo.work_dir() {
                        out.push_str(&format!("work_dir: {}\n", wd));
                    }
                    if let Ok(is_empty) = repo.is_empty() {
                        out.push_str(&format!("is_empty: {}\n", is_empty));
                    }
                    if let Ok(head) = repo.head_id() {
                        out.push_str(&format!("head: {}\n", head));
                    }
                    BuiltinResult {
                        exit_code: 0,
                        stdout: out.into_bytes(),
                        stderr: Vec::new(),
                    }
                }
                Err(e) => BuiltinResult {
                    exit_code: 1,
                    stdout: Vec::new(),
                    stderr: format!("repo status error: {}\n", e).into_bytes(),
                },
            }
        }
        "init" => {
            let target_path = if let Some(p) = subargs.first() {
                cwd.join(p).to_string_lossy().to_string()
            } else {
                cwd.to_string_lossy().to_string()
            };

            let opts = InitOptions {
                bare: Some(false),
                destination_must_be_empty: Some(false),
                open_options: None,
                initial_head: Some("main".to_string()),
                description: None,
                object_format: None,
                compat_object_format: None,
            };

            match Repository::init_opts(target_path.clone(), opts) {
                Ok(_) => BuiltinResult {
                    exit_code: 0,
                    stdout: format!("Initialized repository at {}\n", target_path).into_bytes(),
                    stderr: Vec::new(),
                },
                Err(e) => BuiltinResult {
                    exit_code: 1,
                    stdout: Vec::new(),
                    stderr: format!("repo init error: {}\n", e).into_bytes(),
                },
            }
        }
        "head" => {
            let path_str = cwd.to_string_lossy().to_string();
            match Repository::open(path_str) {
                Ok(repo) => match repo.head_id() {
                    Ok(id) => BuiltinResult {
                        exit_code: 0,
                        stdout: format!("{}\n", id).into_bytes(),
                        stderr: Vec::new(),
                    },
                    Err(e) => BuiltinResult {
                        exit_code: 1,
                        stdout: Vec::new(),
                        stderr: format!("repo head error: {}\n", e).into_bytes(),
                    },
                },
                Err(e) => BuiltinResult {
                    exit_code: 1,
                    stdout: Vec::new(),
                    stderr: format!("repo error: {}\n", e).into_bytes(),
                },
            }
        }
        "is-repo" => {
            let target = if let Some(p) = subargs.first() {
                cwd.join(p).to_string_lossy().to_string()
            } else {
                cwd.to_string_lossy().to_string()
            };
            let is_r = Repository::is_repo(target);
            BuiltinResult {
                exit_code: if is_r { 0 } else { 1 },
                stdout: format!("{}\n", is_r).into_bytes(),
                stderr: Vec::new(),
            }
        }
        "is-bare" => {
            let path_str = cwd.to_string_lossy().to_string();
            match Repository::open(path_str) {
                Ok(repo) => {
                    let bare = repo.is_bare();
                    BuiltinResult {
                        exit_code: 0,
                        stdout: format!("{}\n", bare).into_bytes(),
                        stderr: Vec::new(),
                    }
                }
                Err(e) => BuiltinResult {
                    exit_code: 1,
                    stdout: Vec::new(),
                    stderr: format!("repo error: {}\n", e).into_bytes(),
                },
            }
        }
        unknown => BuiltinResult {
            exit_code: 1,
            stdout: Vec::new(),
            stderr: format!("Unknown subcommand 'repo {}'\n", unknown).into_bytes(),
        },
    }
}
