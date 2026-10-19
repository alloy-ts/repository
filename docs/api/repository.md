# gix Crate API & `Repository` Architecture

This document summarizes the exported API of the Rust `gix` crate (v0.89.0, part of `gitoxide`) and details the design mapping used by this Node.js binding crate (`repository`).

---

## 1. Overview of `gix`

`gix` is the central porcelain crate of `gitoxide`. It exposes high-level, convenient abstractions over low-level plumbing crates (`gix-odb`, `gix-ref`, `gix-pack`, `gix-revision`, `gix-commit`, etc.) without sacrificing performance.

### Core Concept: `Repository` & `ThreadSafeRepository`

In `gix`, repository operations are split between two handle types:

- **`gix::ThreadSafeRepository`**: A thread-safe handle implementing `Send + Sync`. It holds system resources and shared caches loaded when opening or initializing a repository.
- **`gix::Repository`**: A thread-local handle created from a `ThreadSafeRepository` (`thread_safe_repo.to_thread_local()`). It manages thread-local object caches and state.

---

## 2. API Exported by `gix` (v0.89.0)

### Top-Level Free Functions

- `gix::open(path)` / `gix::open_opts(path, options)`: Open an existing repository at `path`.
- `gix::init(path)` / `gix::init_bare(path)`: Initialize a new working-tree or bare repository.
- `gix::discover(path)` / `gix::discover_opts(path, options)`: Discover a repository by walking parent directories.
- `gix::prepare_clone(url, path)` / `gix::prepare_clone_bare(url, path)`: Build a clone builder for fetching from remotes.

### Primary Structs

- **`Repository`**: Thread-local handle providing access to references, object database, configuration, commit walks, diffs, remotes, and worktree.
- **`ThreadSafeRepository`**: Thread-safe container (`Send + Sync`) for cross-thread sharing.
- **`Commit`**: Decoded commit object with access to author/committer signatures, tree, parents, and signature verification (`commit.verify_signature()`).
- **`Tree`**: Decoded tree object containing directory entries (`TreeEntry`).
- **`Blob`**: Raw data blob object.
- **`Tag`**: Decoded annotated tag pointing to a target object.
- **`Object`**: Generic object wrapper for any git object kind (`Commit`, `Tree`, `Blob`, `Tag`).
- **`Reference`**: Named reference (branch, tag, remote tracking branch).
- **`Head`**: `HEAD` reference representation (direct, symbolic, detached, or unborn).
- **`Remote`**: Representation of a remote host configuration, supporting fetch and push operations.

---

## 3. Node.js Binding Mapping & Design Rationale

### Static Factory Methods for `Repository`

`gix` does not have an instance method for `Repository::open` or `Repository::new` — opening or initializing a repository is a fallible free function (`gix::open`, `gix::init`, `gix::discover`).

In `napi-rs`, class constructors (`constructor()`) cannot return Rust errors gracefully or fail fallibly. Therefore, the binding exposes `Repository` as a class with static factory methods:

- `Repository.open(path: string): Repository`
- `Repository.init(path: string, bare?: boolean): Repository`
- `Repository.discover(path?: string): Repository`
- `Repository.clone(url: string, path: string, bare?: boolean): Promise<Repository>` (or `Repository.clone(...)` static constructor)

### Thread Safety & Async Network Operations

The Node.js `Repository` struct wraps `Arc<gix::ThreadSafeRepository>`.

- **Sync operations** (reading commits, refs, trees, diffs) obtain a cheap thread-local handle via `thread_local(&self.inner)`.
- **Async network operations** (`clone`, `fetch`, `push`) run off-thread on libuv worker threads via `napi::Task` / `AsyncTask`, moving `Arc<ThreadSafeRepository>` safely across thread boundaries.

---

## 4. Key Binding APIs

| JS API                              | Rust `gix` Mapping                       | Description                          |
| ----------------------------------- | ---------------------------------------- | ------------------------------------ |
| `Repository.open(path)`             | `gix::ThreadSafeRepository::open`        | Opens repo at `path`                 |
| `Repository.init(path, bare)`       | `gix::init` / `gix::init_bare`           | Initializes new git repo             |
| `Repository.discover(path)`         | `gix::discover`                          | Finds repo in current or parent dirs |
| `Repository.clone(url, path, bare)` | `gix::prepare_clone`                     | Async clone into `path`              |
| `repo.head()` / `repo.headCommit()` | `repo.head()` / `repo.head_commit()`     | Resolves `HEAD`                      |
| `repo.log(rev, limit, firstParent)` | `repo.rev_walk()` + `repo.find_commit()` | Single-call commit history log       |
| `commit.verifySignature()`          | `commit.verify_signature()`              | Verifies cryptographic signature     |
| `commit.sign()`                     | `decoded.sign(Options)`                  | Signs commit with configured key     |
