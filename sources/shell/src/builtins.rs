//! Built-in commands implementation for Bun-compatible shell.
//! Cross-platform native implementations:
//! echo, pwd, cd, ls, rm, cat, touch, mkdir, which, mv, exit, true, false, yes, seq, dirname, basename.

use std::fs;
use std::path::{Path, PathBuf};

pub struct BuiltinResult {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub fn is_builtin(name: &str) -> bool {
    name == "repo"
        || name == "git"
        || fusevm::shell_builtins::is_builtin(name)
        || matches!(
            name,
            "echo"
                | "pwd"
                | "cd"
                | "ls"
                | "rm"
                | "cat"
                | "grep"
                | "wc"
                | "touch"
                | "mkdir"
                | "which"
                | "mv"
                | "exit"
                | "true"
                | "false"
                | "yes"
                | "seq"
                | "dirname"
                | "basename"
                | "awk"
        )
}

pub fn run_builtin(
    name: &str,
    args: &[String],
    stdin_data: &[u8],
    cwd: &Path,
) -> Option<BuiltinResult> {
    match name {
        "repo" | "git" => Some(crate::shell::repo_builtin::builtin_repo(args, cwd)),
        "echo" => Some(builtin_echo(args)),
        "pwd" => Some(builtin_pwd(cwd)),
        "cd" => Some(builtin_cd(args, cwd)),
        "ls" => Some(builtin_ls(args, cwd)),
        "rm" => Some(builtin_rm(args, cwd)),
        "cat" => Some(builtin_cat(args, stdin_data, cwd)),
        "grep" => Some(builtin_grep(args, stdin_data, cwd)),
        "wc" => Some(builtin_wc(args, stdin_data, cwd)),
        "touch" => Some(builtin_touch(args, cwd)),
        "mkdir" => Some(builtin_mkdir(args, cwd)),
        "which" => Some(builtin_which(args)),
        "mv" => Some(builtin_mv(args, cwd)),
        "exit" => Some(builtin_exit(args)),
        "true" => Some(BuiltinResult {
            exit_code: 0,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }),
        "false" => Some(BuiltinResult {
            exit_code: 1,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }),
        "yes" => Some(builtin_yes(args)),
        "seq" => Some(builtin_seq(args)),
        "dirname" => Some(builtin_dirname(args)),
        "basename" => Some(builtin_basename(args)),
        "awk" => Some(builtin_awk(args, stdin_data, cwd)),
        _ => None,
    }
}

fn builtin_echo(args: &[String]) -> BuiltinResult {
    let mut no_newline = false;
    let mut start_idx = 0;
    if let Some(first) = args.first() {
        if first == "-n" {
            no_newline = true;
            start_idx = 1;
        }
    }

    let text = args[start_idx..].join(" ");
    let mut stdout = text.into_bytes();
    if !no_newline {
        stdout.push(b'\n');
    }

    BuiltinResult {
        exit_code: 0,
        stdout,
        stderr: Vec::new(),
    }
}

enum AwkPrintItem {
    Field(usize),
    FieldNF,
    StringLiteral(String),
    VarNR,
    VarNF,
}

fn parse_awk_print_action(body: &str) -> Option<Vec<AwkPrintItem>> {
    let trimmed = body.trim();
    let action_str = if trimmed.starts_with('{') && trimmed.ends_with('}') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    }
    .trim();

    if !action_str.starts_with("print") {
        return None;
    }

    let args_str = action_str[5..].trim();
    if args_str.is_empty() {
        return Some(vec![AwkPrintItem::Field(0)]); // print with no args defaults to print $0
    }

    // Split args by comma respecting quotes
    let mut items = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;

    for c in args_str.chars() {
        match c {
            '"' => {
                in_quote = !in_quote;
                current.push(c);
            }
            ',' if !in_quote => {
                items.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(c),
        }
    }
    if !current.trim().is_empty() {
        items.push(current.trim().to_string());
    }

    let mut print_items = Vec::new();
    for item in items {
        if item.starts_with('"') && item.ends_with('"') && item.len() >= 2 {
            let lit = item[1..item.len() - 1].to_string();
            print_items.push(AwkPrintItem::StringLiteral(lit));
        } else if let Some(field_num_str) = item.strip_prefix('$') {
            if let Ok(idx) = field_num_str.trim().parse::<usize>() {
                print_items.push(AwkPrintItem::Field(idx));
            } else if field_num_str.trim() == "NF" {
                print_items.push(AwkPrintItem::FieldNF);
            } else {
                return None;
            }
        } else if item == "NR" {
            print_items.push(AwkPrintItem::VarNR);
        } else if item == "NF" {
            print_items.push(AwkPrintItem::VarNF);
        } else if let Ok(idx) = item.parse::<usize>() {
            print_items.push(AwkPrintItem::StringLiteral(idx.to_string()));
        } else {
            return None;
        }
    }

    Some(print_items)
}

fn builtin_awk(args: &[String], stdin_data: &[u8], cwd: &Path) -> BuiltinResult {
    let mut field_sep: Option<String> = None;
    let mut script: Option<String> = None;
    let mut files = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "-F" {
            if i + 1 < args.len() {
                field_sep = Some(args[i + 1].clone());
                i += 2;
                continue;
            } else {
                return BuiltinResult {
                    exit_code: 2,
                    stdout: Vec::new(),
                    stderr: b"awk: option requires an argument: -F\n".to_vec(),
                };
            }
        } else if arg.starts_with("-F") && arg.len() > 2 {
            field_sep = Some(arg[2..].to_string());
            i += 1;
            continue;
        } else if script.is_none() {
            script = Some(arg.clone());
            i += 1;
        } else {
            files.push(arg.clone());
            i += 1;
        }
    }

    let script = match script {
        Some(s) => s,
        None => {
            return BuiltinResult {
                exit_code: 2,
                stdout: Vec::new(),
                stderr: b"awk: no program given\n".to_vec(),
            };
        }
    };

    let print_items = match parse_awk_print_action(&script) {
        Some(items) => items,
        None => {
            return BuiltinResult {
                exit_code: 2,
                stdout: Vec::new(),
                stderr: format!("awk: unsupported awk script: {}\n", script).into_bytes(),
            };
        }
    };

    let mut inputs = Vec::new();
    if files.is_empty() || (files.len() == 1 && files[0] == "-") {
        inputs.push(String::from_utf8_lossy(stdin_data).to_string());
    } else {
        for file in &files {
            if file == "-" {
                inputs.push(String::from_utf8_lossy(stdin_data).to_string());
            } else {
                let path = cwd.join(file);
                match fs::read_to_string(&path) {
                    Ok(content) => inputs.push(content),
                    Err(e) => {
                        return BuiltinResult {
                            exit_code: 1,
                            stdout: Vec::new(),
                            stderr: format!("awk: {}: {}\n", file, e).into_bytes(),
                        };
                    }
                }
            }
        }
    }

    let mut stdout = Vec::new();
    let mut nr = 0;

    for content in inputs {
        for line in content.lines() {
            nr += 1;
            let fields: Vec<&str> = if let Some(ref sep) = field_sep {
                line.split(sep.as_str()).collect()
            } else {
                line.split_whitespace().collect()
            };
            let nf = fields.len();

            let mut out_parts = Vec::new();
            for item in &print_items {
                match item {
                    AwkPrintItem::Field(0) => {
                        out_parts.push(line.to_string());
                    }
                    AwkPrintItem::Field(idx) => {
                        if *idx >= 1 && *idx <= nf {
                            out_parts.push(fields[*idx - 1].to_string());
                        } else {
                            out_parts.push(String::new());
                        }
                    }
                    AwkPrintItem::FieldNF => {
                        if nf >= 1 {
                            out_parts.push(fields[nf - 1].to_string());
                        } else {
                            out_parts.push(String::new());
                        }
                    }
                    AwkPrintItem::StringLiteral(lit) => {
                        out_parts.push(lit.clone());
                    }
                    AwkPrintItem::VarNR => {
                        out_parts.push(nr.to_string());
                    }
                    AwkPrintItem::VarNF => {
                        out_parts.push(nf.to_string());
                    }
                }
            }

            stdout.extend_from_slice(out_parts.join(" ").as_bytes());
            stdout.push(b'\n');
        }
    }

    BuiltinResult {
        exit_code: 0,
        stdout,
        stderr: Vec::new(),
    }
}

fn builtin_pwd(cwd: &Path) -> BuiltinResult {
    let path_str = cwd.to_string_lossy().to_string();
    let mut stdout = path_str.into_bytes();
    stdout.push(b'\n');

    BuiltinResult {
        exit_code: 0,
        stdout,
        stderr: Vec::new(),
    }
}

fn builtin_cd(args: &[String], cwd: &Path) -> BuiltinResult {
    let target = if let Some(first) = args.first() {
        if first == "~" {
            dirs_home()
        } else {
            cwd.join(first)
        }
    } else {
        dirs_home()
    };

    if target.is_dir() {
        BuiltinResult {
            exit_code: 0,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    } else {
        BuiltinResult {
            exit_code: 1,
            stdout: Vec::new(),
            stderr: format!("cd: no such file or directory: {}\n", target.display()).into_bytes(),
        }
    }
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"))
}

fn builtin_ls(args: &[String], cwd: &Path) -> BuiltinResult {
    let mut long_format = false;
    let mut all = false;
    let mut paths = Vec::new();

    for arg in args {
        if arg.starts_with('-') && arg.len() > 1 {
            for c in arg.chars().skip(1) {
                if c == 'l' {
                    long_format = true;
                } else if c == 'a' {
                    all = true;
                }
            }
        } else {
            paths.push(arg.as_str());
        }
    }

    let target_paths = if paths.is_empty() {
        vec![cwd.to_path_buf()]
    } else {
        paths.into_iter().map(|p| cwd.join(p)).collect()
    };

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_code = 0;

    for path in target_paths {
        if path.is_file() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            stdout.extend_from_slice(name.as_bytes());
            stdout.push(b'\n');
        } else if let Ok(entries) = fs::read_dir(&path) {
            let mut names = Vec::new();
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !all && name.starts_with('.') {
                    continue;
                }
                names.push((name, entry));
            }
            names.sort_by(|a, b| a.0.cmp(&b.0));

            for (name, entry) in names {
                if long_format {
                    let meta = entry.metadata().ok();
                    let len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                    let line = format!("{:8} {}\n", len, name);
                    stdout.extend_from_slice(line.as_bytes());
                } else {
                    stdout.extend_from_slice(name.as_bytes());
                    stdout.push(b'\n');
                }
            }
        } else {
            exit_code = 1;
            let err = format!(
                "ls: cannot access '{}': No such file or directory\n",
                path.display()
            );
            stderr.extend_from_slice(err.as_bytes());
        }
    }

    BuiltinResult {
        exit_code,
        stdout,
        stderr,
    }
}

fn builtin_rm(args: &[String], cwd: &Path) -> BuiltinResult {
    let mut recursive = false;
    let mut force = false;
    let mut targets = Vec::new();

    for arg in args {
        if arg.starts_with('-') && arg.len() > 1 {
            for c in arg.chars().skip(1) {
                if c == 'r' || c == 'R' {
                    recursive = true;
                } else if c == 'f' {
                    force = true;
                }
            }
        } else {
            targets.push(cwd.join(arg));
        }
    }

    let mut exit_code = 0;
    let mut stderr = Vec::new();

    for target in targets {
        if target.is_dir() {
            if recursive {
                if let Err(e) = fs::remove_dir_all(&target) {
                    if !force {
                        exit_code = 1;
                        stderr.extend_from_slice(
                            format!("rm: {}: {}\n", target.display(), e).as_bytes(),
                        );
                    }
                }
            } else {
                exit_code = 1;
                stderr.extend_from_slice(
                    format!("rm: {}: is a directory\n", target.display()).as_bytes(),
                );
            }
        } else if target.is_file() || target.is_symlink() {
            if let Err(e) = fs::remove_file(&target) {
                if !force {
                    exit_code = 1;
                    stderr
                        .extend_from_slice(format!("rm: {}: {}\n", target.display(), e).as_bytes());
                }
            }
        } else if !force {
            exit_code = 1;
            stderr.extend_from_slice(
                format!("rm: {}: No such file or directory\n", target.display()).as_bytes(),
            );
        }
    }

    BuiltinResult {
        exit_code,
        stdout: Vec::new(),
        stderr,
    }
}

fn builtin_grep(args: &[String], stdin_data: &[u8], cwd: &Path) -> BuiltinResult {
    let mut ignore_case = false;
    let mut invert_match = false;
    let mut pattern: Option<String> = None;
    let mut files = Vec::new();

    for arg in args {
        if arg.starts_with('-') && arg.len() > 1 && !arg.starts_with("--") {
            for c in arg.chars().skip(1) {
                match c {
                    'i' => ignore_case = true,
                    'v' => invert_match = true,
                    _ => {}
                }
            }
        } else if pattern.is_none() {
            pattern = Some(arg.clone());
        } else {
            files.push(arg.as_str());
        }
    }

    let pat = match pattern {
        Some(p) => {
            if ignore_case {
                p.to_lowercase()
            } else {
                p
            }
        }
        None => {
            return BuiltinResult {
                exit_code: 2,
                stdout: Vec::new(),
                stderr: b"grep: missing pattern\n".to_vec(),
            };
        }
    };

    let check_line = |line: &str| -> bool {
        let matched = if ignore_case {
            line.to_lowercase().contains(&pat)
        } else {
            line.contains(&pat)
        };
        if invert_match {
            !matched
        } else {
            matched
        }
    };

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_code = 1; // 1 if no line was selected

    if files.is_empty() || (files.len() == 1 && files[0] == "-") {
        let text = String::from_utf8_lossy(stdin_data);
        for line in text.lines() {
            if check_line(line) {
                exit_code = 0;
                stdout.extend_from_slice(line.as_bytes());
                stdout.push(b'\n');
            }
        }
        return BuiltinResult {
            exit_code,
            stdout,
            stderr,
        };
    }

    for file in &files {
        let path = cwd.join(file);
        match fs::read_to_string(&path) {
            Ok(content) => {
                for line in content.lines() {
                    if check_line(line) {
                        exit_code = 0;
                        if files.len() > 1 {
                            stdout.extend_from_slice(format!("{}:{}\n", file, line).as_bytes());
                        } else {
                            stdout.extend_from_slice(format!("{}\n", line).as_bytes());
                        }
                    }
                }
            }
            Err(e) => {
                stderr.extend_from_slice(format!("grep: {}: {}\n", file, e).as_bytes());
            }
        }
    }

    BuiltinResult {
        exit_code,
        stdout,
        stderr,
    }
}

fn builtin_cat(args: &[String], stdin_data: &[u8], cwd: &Path) -> BuiltinResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_code = 0;

    if args.is_empty() || (args.len() == 1 && args[0] == "-") {
        stdout.extend_from_slice(stdin_data);
        return BuiltinResult {
            exit_code: 0,
            stdout,
            stderr,
        };
    }

    for arg in args {
        if arg == "-" {
            stdout.extend_from_slice(stdin_data);
        } else {
            let path = cwd.join(arg);
            match fs::read(&path) {
                Ok(bytes) => stdout.extend_from_slice(&bytes),
                Err(e) => {
                    exit_code = 1;
                    stderr
                        .extend_from_slice(format!("cat: {}: {}\n", path.display(), e).as_bytes());
                }
            }
        }
    }

    BuiltinResult {
        exit_code,
        stdout,
        stderr,
    }
}

fn builtin_wc(args: &[String], stdin_data: &[u8], cwd: &Path) -> BuiltinResult {
    let mut count_lines = false;
    let mut count_words = false;
    let mut count_bytes = false;
    let mut files = Vec::new();

    for arg in args {
        if arg.starts_with('-') && arg.len() > 1 {
            for c in arg.chars().skip(1) {
                match c {
                    'l' => count_lines = true,
                    'w' => count_words = true,
                    'c' | 'm' => count_bytes = true,
                    _ => {}
                }
            }
        } else {
            files.push(arg.as_str());
        }
    }

    if !count_lines && !count_words && !count_bytes {
        count_lines = true;
        count_words = true;
        count_bytes = true;
    }

    let compute_stats = |data: &[u8]| -> (usize, usize, usize) {
        let lines = data.iter().filter(|&&b| b == b'\n').count();
        let text = String::from_utf8_lossy(data);
        let words = text.split_whitespace().count();
        let bytes = data.len();
        (lines, words, bytes)
    };

    let format_counts = |lines: usize, words: usize, bytes: usize, label: &str| -> String {
        let mut parts = Vec::new();
        if count_lines {
            parts.push(format!("{:>4}", lines));
        }
        if count_words {
            parts.push(format!("{:>4}", words));
        }
        if count_bytes {
            parts.push(format!("{:>4}", bytes));
        }
        if label.is_empty() {
            parts.join(" ")
        } else {
            format!("{} {}", parts.join(" "), label)
        }
    };

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_code = 0;

    if files.is_empty() || (files.len() == 1 && files[0] == "-") {
        let (lines, words, bytes) = compute_stats(stdin_data);
        let line = format!("{}\n", format_counts(lines, words, bytes, ""));
        stdout.extend_from_slice(line.as_bytes());
        return BuiltinResult {
            exit_code: 0,
            stdout,
            stderr,
        };
    }

    for file in &files {
        if *file == "-" {
            let (l, w, b) = compute_stats(stdin_data);
            stdout.extend_from_slice(format!("{}\n", format_counts(l, w, b, "-")).as_bytes());
        } else {
            let path = cwd.join(file);
            match fs::read(&path) {
                Ok(data) => {
                    let (l, w, b) = compute_stats(&data);
                    stdout.extend_from_slice(format!("{}\n", format_counts(l, w, b, file)).as_bytes());
                }
                Err(e) => {
                    exit_code = 1;
                    stderr.extend_from_slice(format!("wc: {}: {}\n", file, e).as_bytes());
                }
            }
        }
    }

    BuiltinResult {
        exit_code,
        stdout,
        stderr,
    }
}

fn builtin_touch(args: &[String], cwd: &Path) -> BuiltinResult {
    let mut exit_code = 0;
    let mut stderr = Vec::new();

    for arg in args {
        let path = cwd.join(arg);
        if let Err(e) = fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path) {
            exit_code = 1;
            stderr.extend_from_slice(format!("touch: {}: {}\n", path.display(), e).as_bytes());
        }
    }

    BuiltinResult {
        exit_code,
        stdout: Vec::new(),
        stderr,
    }
}

fn builtin_mkdir(args: &[String], cwd: &Path) -> BuiltinResult {
    let mut parents = false;
    let mut targets = Vec::new();

    for arg in args {
        if arg == "-p" {
            parents = true;
        } else {
            targets.push(cwd.join(arg));
        }
    }

    let mut exit_code = 0;
    let mut stderr = Vec::new();

    for target in targets {
        let res = if parents {
            fs::create_dir_all(&target)
        } else {
            fs::create_dir(&target)
        };

        if let Err(e) = res {
            exit_code = 1;
            stderr.extend_from_slice(format!("mkdir: {}: {}\n", target.display(), e).as_bytes());
        }
    }

    BuiltinResult {
        exit_code,
        stdout: Vec::new(),
        stderr,
    }
}

fn builtin_which(args: &[String]) -> BuiltinResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_code = 0;

    let path_var = std::env::var("PATH").unwrap_or_default();
    let dirs: Vec<&str> = path_var.split(':').collect();

    for arg in args {
        let mut found = false;
        for dir in &dirs {
            let p = Path::new(dir).join(arg);
            if p.is_file() {
                stdout.extend_from_slice(p.to_string_lossy().as_bytes());
                stdout.push(b'\n');
                found = true;
                break;
            }
        }
        if !found {
            exit_code = 1;
            stderr.extend_from_slice(format!("{}: not found\n", arg).as_bytes());
        }
    }

    BuiltinResult {
        exit_code,
        stdout,
        stderr,
    }
}

fn builtin_mv(args: &[String], cwd: &Path) -> BuiltinResult {
    if args.len() < 2 {
        return BuiltinResult {
            exit_code: 1,
            stdout: Vec::new(),
            stderr: b"mv: missing destination file operand\n".to_vec(),
        };
    }

    let sources = &args[..args.len() - 1];
    let dest = cwd.join(&args[args.len() - 1]);

    let mut exit_code = 0;
    let mut stderr = Vec::new();

    if sources.len() > 1 && !dest.is_dir() {
        return BuiltinResult {
            exit_code: 1,
            stdout: Vec::new(),
            stderr: format!("mv: target '{}' is not a directory\n", dest.display()).into_bytes(),
        };
    }

    for src in sources {
        let src_path = cwd.join(src);
        let target_path = if dest.is_dir() {
            dest.join(src_path.file_name().unwrap_or_default())
        } else {
            dest.clone()
        };

        if let Err(e) = fs::rename(&src_path, &target_path) {
            exit_code = 1;
            stderr.extend_from_slice(format!("mv: {}: {}\n", src_path.display(), e).as_bytes());
        }
    }

    BuiltinResult {
        exit_code,
        stdout: Vec::new(),
        stderr,
    }
}

fn builtin_exit(args: &[String]) -> BuiltinResult {
    let code = args
        .first()
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(0);
    BuiltinResult {
        exit_code: code,
        stdout: Vec::new(),
        stderr: Vec::new(),
    }
}

fn builtin_yes(args: &[String]) -> BuiltinResult {
    let msg = if args.is_empty() { "y" } else { &args[0] };
    // Generate a reasonable finite chunk for shell buffer
    let mut stdout = Vec::new();
    for _ in 0..100 {
        stdout.extend_from_slice(msg.as_bytes());
        stdout.push(b'\n');
    }
    BuiltinResult {
        exit_code: 0,
        stdout,
        stderr: Vec::new(),
    }
}

fn builtin_seq(args: &[String]) -> BuiltinResult {
    if args.is_empty() {
        return BuiltinResult {
            exit_code: 1,
            stdout: Vec::new(),
            stderr: b"seq: missing operand\n".to_vec(),
        };
    }

    let (start, step, end) = match args.len() {
        1 => (1i64, 1i64, args[0].parse::<i64>().unwrap_or(1)),
        2 => (
            args[0].parse::<i64>().unwrap_or(1),
            1i64,
            args[1].parse::<i64>().unwrap_or(1),
        ),
        _ => (
            args[0].parse::<i64>().unwrap_or(1),
            args[1].parse::<i64>().unwrap_or(1),
            args[2].parse::<i64>().unwrap_or(1),
        ),
    };

    let mut stdout = Vec::new();
    if step > 0 {
        let mut curr = start;
        while curr <= end {
            stdout.extend_from_slice(format!("{}\n", curr).as_bytes());
            curr += step;
        }
    } else if step < 0 {
        let mut curr = start;
        while curr >= end {
            stdout.extend_from_slice(format!("{}\n", curr).as_bytes());
            curr += step;
        }
    }

    BuiltinResult {
        exit_code: 0,
        stdout,
        stderr: Vec::new(),
    }
}

fn builtin_dirname(args: &[String]) -> BuiltinResult {
    if args.is_empty() {
        return BuiltinResult {
            exit_code: 1,
            stdout: Vec::new(),
            stderr: b"dirname: missing operand\n".to_vec(),
        };
    }

    let mut stdout = Vec::new();
    for arg in args {
        let p = Path::new(arg);
        let dir = p.parent().unwrap_or_else(|| Path::new("."));
        let dir_str = if dir.as_os_str().is_empty() {
            "."
        } else {
            dir.to_str().unwrap_or(".")
        };
        stdout.extend_from_slice(format!("{}\n", dir_str).as_bytes());
    }

    BuiltinResult {
        exit_code: 0,
        stdout,
        stderr: Vec::new(),
    }
}

fn builtin_basename(args: &[String]) -> BuiltinResult {
    if args.is_empty() {
        return BuiltinResult {
            exit_code: 1,
            stdout: Vec::new(),
            stderr: b"basename: missing operand\n".to_vec(),
        };
    }

    let p = Path::new(&args[0]);
    let name = p.file_name().unwrap_or_default().to_string_lossy();
    let res = if args.len() > 1 && name.ends_with(&args[1]) {
        &name[..name.len() - args[1].len()]
    } else {
        &name
    };

    BuiltinResult {
        exit_code: 0,
        stdout: format!("{}\n", res).into_bytes(),
        stderr: Vec::new(),
    }
}
