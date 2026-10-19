# `gix` Crate & `Repository` API Overview

This document reviews the `gix` (gitoxide) crate API (version `0.89.0`) as documented on [docs.rs/gix/0.89.0](https://docs.rs/gix/0.89.0/gix/) and describes how its capabilities are exposed through Node.js / TypeScript bindings.

---

## 1. Overview of `gix`

`gix` is the main high-level "porcelain" crate in the `gitoxide` project. It provides a `Repository` abstraction that acts as a central hub for all Git operations, delegating low-level tasks to specialized plumbing crates (such as `gix-odb`, `gix-ref`, `gix-config`, `gix-revision`, `gix-actor`, and `gix-object`).

Key characteristics:

- **Thread-local vs. Thread-safe handles:** `gix::Repository` is a thread-local handle designed for fast, single-threaded operations (holding caches and non-`Sync` state). `gix::ThreadSafeRepository` is `Send + Sync` and can be safely shared across threads.
- **Trust model:** Integrates file ownership check (`gix_sec::Trust`) to avoid executing untrusted executables or reading untrusted config files.
- **Batteries-included feature flags:** Fine-grained feature flags optimize compile times and feature capabilities.

---

## 2. Exported Crate API Surface (`gix` v0.89.0)

### Primary Structs & Types

- **`Repository`**: Thread-local handle to interact with a Git repository.
- **`ThreadSafeRepository`**: Multi-threaded handle that implements `Send + Sync`.
- **`Commit<'repo>`**: Decoded commit object with access to its owning repository.
- **`Tree<'repo>`**: Decoded tree object containing directory entries.
- **`Blob<'repo>`**: Lazy handle to un-typed binary object data.
- **`Tag<'repo>`**: Decoded annotated tag object.
- **`Object<'repo>`**: Downcastable wrapper for any Git object type.
- **`Reference<'repo>`**: A Git reference pointing to an object ID or another reference.
- **`Head<'repo>`**: Represents `.git/HEAD` and its possible states (attached to branch, detached, born, uninitialized).
- **`Remote<'repo>`**: Handle to interact with configured remote hosts for clone, fetch, and push.
- **`Url`**: URL abstraction with support for Git protocols and SSH/HTTP endpoints.

### Top-Level Free Functions

- **Opening / Discovering:** `gix::open(path)`, `gix::open_opts(...)`, `gix::discover(path)`, `gix::discover_opts(...)`
- **Initializing:** `gix::init(path)`, `gix::init_bare(path)`
- **Cloning:** `gix::prepare_clone(url, path)`, `gix::prepare_clone_bare(url, path)`
- **Config:** `gix::config()`, `gix::config_mut()`, `gix::config_path()`

---

## 3. Core Operations in `gix::Repository`

### Repository Construction (Static Constructors)

In Rust `gix`, creation is performed via top-level functions or `ThreadSafeRepository` associated functions:

```rust
let repo = gix::open(".")?;
let bare_repo = gix::init_bare("./bare.git")?;
let discovered = gix::discover("./some/subfolder")?;
let prepare = gix::prepare_clone("https://github.com/org/repo.git", "./target")?;
```

### HEAD & Object Access

```rust
let head = repo.head()?;
let head_commit = repo.head_commit()?;
let commit = repo.find_commit(id)?;
let tree = repo.find_tree(tree_id)?;
let blob = repo.find_blob(blob_id)?;
let tag = repo.find_tag(tag_id)?;
```

### References & Tags

```rust
let refs = repo.references()?;
let r = repo.find_reference("refs/heads/main")?;
repo.tag("v1.0.0", target_id, kind, tagger, "release message", constraint)?;
repo.tag_reference("v1.0-lightweight", target_id, constraint)?;
```

### Revisions & History Walking

```rust
let id = repo.rev_parse_single("HEAD~1")?;
let walk = repo.rev_walk([id]);
for item in walk.all()? {
    let id = item?.id;
}
let base = repo.merge_base(commit_a, commit_b)?;
```

### Commit Verification and Signing (`command` Feature)

When the `command` feature flag is enabled:

- `commit.verify_signature()`: Invokes external GPG/GPGSM/SSH-keygen programs based on Git config and returns `gix::commit::verify::Outcome` with status, trust level, signer identity, key ID, and human-readable output.
- `commit.signed()`: Creates a signed copy of the commit using configured `user.signingKey` and `gpg.format`, writing the resulting signed commit object into the repository.

---

## 4. Feature Flags Breakdown

| Feature Flag              | Purpose in `gix`                                                                                                                    | Enabled in this binding |
| ------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- | ----------------------- |
| `command`                 | External CLI tool execution for GPG/SSH commit signature verification and signing (`commit.verify_signature()`, `commit.signed()`). | **Yes**                 |
| `revision`                | Revision specification parsing (`rev_parse_single`), commit walking (`rev_walk`), and git describe operations.                      | **Yes**                 |
| `merge`                   | Standard three-way file and commit merge base operations.                                                                           | **Yes**                 |
| `blob-diff`               | Line-by-line diffing for blobs and tree-to-tree diffs.                                                                              | **Yes**                 |
| `attributes`              | `.gitattributes` and `.gitignore` parsing, pathspec matching, submodules.                                                           | **Yes**                 |
| `status`                  | Worktree and index status inspection.                                                                                               | **Yes**                 |
| `worktree-mutation`       | Worktree creation, removal, and modifications.                                                                                      | **Yes**                 |
| `blocking-network-client` | Synchronous network transfer capabilities used in async background threads for fetch/clone/push.                                    | **Yes**                 |

---

## 5. Design Rationale & Node.js Binding Mapping

### Why Static Constructors (`Repository.open`, `Repository.init`, etc.)?

In `gix`, repository creation is fallible (`Result<Repository, Error>`). napi-rs JavaScript class constructors (`constructor()`) cannot easily return rich error results or handle asynchronous thread pool execution for network cloning.

Thus, all repository instantiation methods are exposed as static factory methods on the `Repository` class:

- `Repository.open(path: string): Repository`
- `Repository.init(path: string, bare?: boolean): Repository`
- `Repository.discover(path?: string): Repository`
- `Repository.clone(url: string, path: string, bare?: boolean): Promise<Repository>`

### Thread Safety Rationale (`ThreadSafeRepository` + `Arc`)

`napi-rs` exposes JS classes to Node.js. For synchronous calls, we maintain an `Arc<gix::ThreadSafeRepository>` internally. Synchronous methods create a thread-local view (`repo.to_thread_local()`) for fast object and ref lookups, while asynchronous operations (like `clone` and `remote.fetch`) easily pass the `Arc<ThreadSafeRepository>` across libuv thread-pool worker threads.
