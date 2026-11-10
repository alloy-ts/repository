# Shell API Documentation

The `shell` module provides a cross-platform, Bun-compatible shell scripting environment using `fusevm` VM and native async process execution, along with `node:child_process` alias APIs.

## Overview

- **Bun-Compatible Shell (`$`)**: Tagged template literal syntax for executing commands with built-in escaping, redirections, and pipes.
- **Built-in Commands**: Native cross-platform implementations for:
  - `awk`: Column/field extraction supporting `-F` separator, `{print $N, ...}`, `$0`, `$NF`, `NR`, `NF`.
  - `cat`: Output file or stdin streams.
  - `grep`: Line filtering supporting `-i` (case-insensitive) and `-v` (invert-match).
  - `wc`: Word, line (`-l`), byte (`-c`), and word (`-w`) counting.
  - `echo`: Print arguments with `-n` (omit trailing newline) support.
  - `pwd`: Print current working directory.
  - `cd`: Change directory (`~` expansion supported).
  - `ls`: List directory contents (`-l` long format, `-a` hidden files).
  - `rm`: Remove files and directories (`-r` recursive, `-f` force).
  - `touch`: Update file timestamp or create empty file.
  - `mkdir`: Create directories (`-p` parents).
  - `which`: Locate executables on `PATH`.
  - `mv`: Move/rename files and directories.
  - `seq`: Generate sequence of numbers (`seq [first [step]] last`).
  - `yes`: Output repetitive text strings.
  - `dirname`: Extract directory component from path.
  - `basename`: Extract filename component from path (with optional suffix stripping).
  - `true` / `false`: Return status code 0 and 1 respectively.
  - `exit`: Terminate command pipeline with numeric code.
- **Repository Integration**: Native `repo` command (and its `git` alias) hooked directly into `src/repo/mod.rs` via `Repository` to inspect and manage Git repositories directly from shell pipelines (`status`, `init`, `head`, `is-repo`, `is-bare`).
- **fusevm Integration**: Shell evaluation, brace expansion (`$.braces`), and `ShellHost` callbacks powered by `fusevm`.
- **`node:child_process` Aliases**: `exec`, `execSync`, `execFile`, `execFileSync`, `spawn`, `spawnSync` backed by `src/task/process.rs`.

---

## Getting Started

```ts
import { $ } from "@alloy-ts/repository";

// Basic command
const out = await $`echo "Hello World!"`.text();
console.log(out.trim()); // "Hello World!"

// Piped execution
const lines = await $`seq 1 5 | cat`.text();

// Reading JSON
const data = await $`echo '{"version": 1}'`.json();
```

---

## Features

### Output Formatting

Commands return a `ShellPromise` with chainable utilities:

- `.text()`: Resolves stdout as a UTF-8 string.
- `.json<T>()`: Parses stdout as JSON.
- `.lines()`: Returns an async iterable yielding output line-by-line.
- `.blob()`: Resolves stdout as a `Blob`.
- `.quiet()`: Suppresses echoing stdout/stderr to parent streams.

```ts
for await (const line of $`seq 1 3`.lines()) {
  console.log(line);
}
```

### Error Handling & Configuration

By default, non-zero exit codes throw a `ShellError`.

- `.nothrow()` / `.throws(false)`: Disables throwing, returning `{ stdout, stderr, exitCode }`.
- `.cwd(dir)`: Sets working directory for the command.
- `.env(record)`: Overrides environment variables for the command.
- Global defaults can be set on `$`: `$.cwd("/tmp")`, `$.nothrow()`, `$.env({ FOO: "bar" })`.

```ts
const { exitCode } = await $`false`.nothrow();
console.log(exitCode); // 1
```

### Built-in `repo` and `git` commands

The shell includes native built-ins `repo` and `git` hooked directly into the gix-backed `Repository` engine (`src/repo/mod.rs`):

- `repo status` / `git status`: Prints repo root, worktree, empty state, and HEAD.
- `repo is-repo [path]` / `git is-repo [path]`: Tests if a path is a Git repository.
- `repo head` / `git head`: Resolves current HEAD commit ID.
- `repo init [path]` / `git init [path]`: Initializes a repository.
- `repo is-bare` / `git is-bare`: Checks if the repository is bare.

```ts
const gitDir = await $`git status`.text();
const isGit = await $`git is-repo`.text();
```

### Utilities

- `$.braces(pattern)`: Expands brace patterns like `echo {1,2,3}` into `["echo 1", "echo 2", "echo 3"]` via `fusevm`.
- `$.escape(str)`: Escapes strings to prevent shell injection.

---

## `node:child_process` Aliases

Compatible drop-in functions for Node's `child_process`:

```ts
import { exec, execSync, execFile, execFileSync, spawn, spawnSync } from "@alloy-ts/repository";

// Async exec
const res = await exec("echo hello");
console.log(res.stdout.toString());

// Synchronous execSync
const buf = execSync("echo hello");

// execFile & spawn
const fileRes = await execFile("echo", ["arg"]);
const spawnRes = await spawn("echo", ["arg"]);
```
