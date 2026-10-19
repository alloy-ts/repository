# `gix` Crate API & Node.js Binding Architecture

This document details the API exported by the [`gix`](https://docs.rs/gix/latest/gix/) crate (gitoxide) and how it is exposed via `napi-rs` to Node.js/TypeScript in this repository.

---

## 1. Top-Level `gix` Crate Surface

`gix` is the high-level, ergonomic interface to Git repositories in Rust, building on lower-level plumbing crates like `gix-odb`, `gix-ref`, `gix-pack`, `gix-config`, etc.

### Free-Standing Factory Functions
* **`gix::open(path)`**: Opens a Git repository at `path` (or auto-discovers `.git` inside worktrees).
* **`gix::init(path)`**: Initializes a new non-bare Git repository with a working tree.
* **`gix::init_bare(path)`**: Initializes a new bare Git repository without a working tree.
* **`gix::discover(path)`**: Searches upward from `path` to discover an enclosing `.git` directory and open it.
* **`gix::prepare_clone(url, path)`**: Prepares a clone operation into `path`, returning a builder to configure transport, options, and execute fetch/checkout.
* **`gix::prepare_clone_bare(url, path)`**: Prepares a bare clone operation into `path`.

---

## 2. Struct Architecture: `ThreadSafeRepository` vs `Repository`

`gix` separates thread-safe storage handles from thread-local operational views:

### `gix::ThreadSafeRepository`
* **Purpose**: Immutable, shareable reference-counted core holding open object database (ODB) handles, pack indexes, and reference database handles.
* **Thread Safety**: Implements `Send` and `Sync`. Safe to wrap in `Arc<ThreadSafeRepository>` and pass across thread boundaries (e.g., to libuv background worker threads for async clone/fetch/push/checkout/status).

### `gix::Repository`
* **Purpose**: Thread-local view created from a `ThreadSafeRepository` via `ts_repo.to_thread_local()` or `ts_repo.into()`.
* **Behavior**: Fast, cache-friendly handle holding thread-local object caches and state. It is `!Send` / `!Sync` due to thread-local caching structures.
* **Usage in Node Bindings**: Each sync wrapper method takes `&Arc<ThreadSafeRepository>`, creates a temporary thread-local `gix::Repository` via `thread_local(&self.inner)`, and executes the operation efficiently.

---

## 3. Core Domain Structs & Handles

When interacting with a `gix::Repository`, operations yield rich handle objects:

| Domain Object | `gix` Struct | Description / Key Capabilities |
|---|---|---|
| **`Commit`** | `gix::Commit<'repo>` | Decodes commit objects (`author`, `committer`, `message`, `tree_id`, `parents`, `decode()`). Provides cryptographic signing (`sign`) and signature verification (`verify_signature`). |
| **`Tree`** | `gix::Tree<'repo>` | Decodes directory trees and lists entries (`TreeEntry`: name, OID, mode). |
| **`Blob`** | `gix::Blob<'repo>` | Accesses raw file content bytes (`data`). |
| **`Tag`** | `gix::Tag<'repo>` | Decodes annotated tag objects (`tagger`, `target_id`, `target_kind`, `message`). |
| **`Reference`** | `gix::Reference<'repo>` | Ref pointers (`refs/heads/*`, `refs/tags/*`). Supports peeling (`peel_to_id`) and deletion (`delete`). |
| **`Head`** | `gix::Head<'repo>` | Special handle for `HEAD`. Reports whether `HEAD` is attached/detached/unborn, referent branch name, and peeled OID. |
| **`Remote`** | `gix::Remote<'repo>` | Configured git remote (`origin`, etc.). Exposes `connect(direction)` to prepare fetch/push network operations. |

---

## 4. Primary API Operations

### Log & Revision Walking
* `repo.rev_walk([tip_ids])`: Instantiates a revision walker iterating backwards through commit history.
* `repo.rev_parse_single(spec)` / `repo.rev_parse_hex(spec)`: Resolves revision specifications (e.g. `HEAD`, `HEAD~1`, `main`, `<sha>`) to an `ObjectId`.
* `repo.merge_base(a, b)` / `repo.merge_base_octopus(ids)`: Computes common ancestor commits.
* `repo.is_ancestor(a, b)`: Checks if a commit is an ancestor of another.
* `repo.describe(id?)`: Generates human-readable descriptions for commits (git describe).

### Object Database (ODB) & Writing
* `repo.find_commit(id)` / `repo.find_tree(id)` / `repo.find_blob(id)` / `repo.find_tag(id)`: Look up decoded objects by OID.
* `repo.find_object(id)` / `repo.try_find_object(id)`: Look up an object of arbitrary type.
* `repo.write_blob(data)` / `repo.write_tree(entries)` / `repo.write_commit(input)` / `repo.write_tag(input)`: Write loose objects directly to the OID store.
* `repo.peel(id, kind)`: Peels tags or commits to a target object kind (`commit`, `tree`, `blob`, `tag`).

### Index & Status
* `repo.index_entries()` / `repo.index_or_load_from_head_entries()`: Reads index entries.
* `repo.edit_index(entries)`: Modifies index entries in memory and persists to `.git/index`.
* `repo.status()`: Computes working tree status asynchronously on the libuv thread pool.

### Merges & Resets
* `repo.merge_trees(ours, theirs, ancestor)`: Merges two trees using a common ancestor.
* `repo.merge_commits(ours, theirs)`: Merges two commit trees and reports conflict status.
* `repo.reset(target, hard?)`: Resets HEAD and index (and working tree if `hard: true`).
* `repo.checkout(target)`: Asynchronously checks out a target commit.

### Attributes, Excludes & Submodules
* `repo.attributes(path)`: Retrieves assigned git attributes for a path.
* `repo.is_path_excluded(path)`: Checks if a path is ignored by `.gitignore`.
* `repo.submodules()` / `repo.submodule_by_name(name)`: Lists and inspects submodules.
* `repo.find_note(target_id)`: Retrieves attached git notes.

---

## 5. Node.js Binding Mapping Rationale

1. **Static Factories vs Constructors**: `napi-rs` constructors (`constructor()`) cannot return errors cleanly or perform asynchronous fallible creation. Therefore, `Repository` exposes static factory methods (`Repository.open`, `Repository.init`, `Repository.discover`, `Repository.clone`) that throw idiomatic JS `Error` objects on failure.
2. **Thread Offloading**: Heavy operations like `clone()`, `remote.fetch()`, `status()`, and `checkout()` run asynchronously on libuv worker threads via `napi::Task` / `AsyncTask`, returning JavaScript `Promise` instances without blocking the main event loop.

---

## 6. Reading Files from Specific Revisions (SHA / Branch / HEAD)

Rather than reading files directly from the working tree directory on disk, `Repository` provides a complete revision API (`revParseSingle`, `findCommit`, `findTree`, `findBlob`) to inspect files as they exist at any commit SHA or branch reference (e.g. `main` or `HEAD`).

### Example: Reading `ReadMe.md` or `repository.manifest.yml` from `main` or SHA in JavaScript

```ts
import { Repository } from "repository";

const repo = Repository.open(".");

// 1. Resolve any revision spec (branch name like "main", "HEAD", or commit SHA)
const commitId = repo.revParseSingle("main"); // or repo.revParseSingle("a266f38...")

// 2. Fetch the commit and its root tree
const commit = repo.findCommit(commitId);
const tree = repo.findTree(commit.treeId);

// 3. Find the target file entry in the tree
const entry = tree.entry("ReadMe.md"); // or tree.entry("repository.manifest.yml")
if (entry) {
  // 4. Retrieve blob content by OID
  const blob = repo.findBlob(entry.id);
  const content = blob.content().toString("utf-8");
  console.log("File content from revision:", content);
}
```

### Manifest Helper (`setup_remotes_from_manifest`)
In Rust (`src/util.rs`), `setup_remotes_from_manifest(repo, revision_or_path)` accepts a revision string (e.g., `"HEAD"` or `"main"`). If a path on disk exists, it reads from disk; otherwise it resolves the revision using `gix::Repository::rev_parse_single`, extracts `repository.manifest.yml` from the commit tree, and sets up configured remotes.
