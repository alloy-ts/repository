# `gix` Crate & Repository API Reference

This document reviews the API exported by the `gix` crate (v0.89.0), focusing on the `Repository` and `ThreadSafeRepository` abstractions, exported structs, factory functions, and JS/TS bindings.

---

## 1. Thread-Safety Model & Architecture

In `gix` (Gitoxide), repository access is split into two primary types:

* **`gix::ThreadSafeRepository`**:
  * An immutable, thread-safe handle (`Send + Sync`) to a Git repository.
  * Designed to be wrapped in an `Arc<ThreadSafeRepository>` and shared across worker threads or asynchronous tasks (e.g., libuv worker threads in Node.js / napi-rs).
* **`gix::Repository`**:
  * A lightweight, thread-local handle created from `ThreadSafeRepository::to_thread_local()`.
  * Provides stateful, ergonomic operations (caching object databases, config snapshots, reference transactions).

In our Node.js bindings, the JavaScript class `Repository` wraps `Arc<gix::ThreadSafeRepository>`. Synchronous JS methods obtain a thread-local view on demand, while async network operations offload tasks to thread pool workers using the `Arc`.

---

## 2. Instantiation & Static Constructors

The `gix` crate exports high-level factory functions for opening, initializing, discovering, and cloning repositories:

| Rust Function | JS Static Method | Description |
| :--- | :--- | :--- |
| `gix::open(path)` / `ThreadSafeRepository::open` | `Repository.open(path)` | Opens an existing Git repository at `path` (expands `.git` for worktrees). |
| `gix::init(path)` | `Repository.init(path, bare=false)` | Initializes a non-bare Git repository with a working directory. |
| `gix::init_bare(path)` | `Repository.init(path, bare=true)` | Initializes a bare Git repository. |
| `gix::discover(path)` | `Repository.discover(path)` | Discovers a repository by traversing parent directories starting at `path`. |
| `gix::prepare_clone(url, path)` | `Repository.clone(url, path, bare=false)` | Clones a repository from a remote URL asynchronously. |
| `gix::prepare_clone_bare(url, path)` | `Repository.clone(url, path, bare=true)` | Clones a bare repository from a remote URL asynchronously. |

---

## 3. Core Exported Structs & API Surface

### 3.1 HEAD & Object Access
* **`Head`**: Represents `HEAD` state (`detached`, `unborn`, or `symbolic` pointing to a reference).
* **`Commit`**: Decoded commit object. Exposes `id`, `message`, `author`, `committer`, `treeId`, `parentIds`, `parents()`, `tree()`, `verifySignature()`, and `sign()`.
* **`Tree`**: Decoded directory tree. Exposes `id`, `entries` (`TreeEntry` array with `name`, `id`, `mode`), and `entry(name)`.
* **`Blob`**: Raw binary content wrapper (`content()` returns a `Buffer`).
* **`Tag`**: Annotated tag object (`id`, `name`, `targetId`, `targetKind`, `tagger`, `message`).
* **`GitObject`**: Kinded object wrapper with downcast helpers (`asCommit()`, `asTree()`, `asBlob()`, `asTag()`).

### 3.2 References & Tags
* **`Reference`**: Represents a branch, tag, or note (`name`, `target`, `kind`, `peel()`, `delete()`).
* **Methods on `Repository`**:
  * `references()`: Returns all references in the repository.
  * `findReference(name)`: Finds a reference by name (e.g. `HEAD`, `refs/heads/main`).
  * `createReference(name, target, force)`: Creates or updates a reference.
  * `createTag(name, target, message, force)`: Creates an annotated tag object and reference.
  * `createLightweightTag(name, target, force)`: Creates a lightweight tag reference.

### 3.3 Remotes
* **`Remote`**: Encapsulates a configured remote (`name`, `url`, `fetch()`).
* **Methods on `Repository`**:
  * `remotes()`: Lists all configured named remotes.
  * `findRemote(name)`: Finds a named remote (e.g. `origin`).

### 3.4 Config & Paths
* **Methods on `Repository`**:
  * `configString(key)`: Queries a single configuration key.
  * `workDir()`: Returns the working directory path (`null` if bare).
  * `gitDir()`: Returns the `.git` directory path.
  * `commonDir()`: Returns the common Git directory path.
  * `isBare()`: Returns boolean indicating whether the repository is bare.
  * `kind()`: Returns the repository kind (`"Common"`, `"Bare"`, etc.).
  * `objectHash()`: Returns `"Sha1"` or `"Sha256"`.

### 3.5 Revisions, History & Log
* **`log(rev?, limit?, firstParent?)`**: High-performance commit history reader returning `LogEntry[]` in reverse-chronological order.
* **`revParseSingle(spec)`**: Resolves a single revision specification (e.g., `HEAD`, `main`, `HEAD~1`, `<sha>`).
* **`revWalk(tips)`**: Walks reachable commit IDs starting from tip SHAs.
* **`mergeBase(one, two)`**: Calculates the merge base between two commits.
* **`mergeBaseOctopus(commits)`**: Calculates the merge base across multiple commits.

### 3.6 Commit Signing & Verification
* **`commit.verifySignature()`**: Verifies a commit signature against configured Git verifiers (`gpg`, `gpgsm`, `ssh-keygen`).
* **`commit.sign()`**: Creates a signed commit object using the configured `user.signingKey` and `gpg.format`.

---

## 4. JavaScript Examples

### Reading Git Logs

```javascript
import { Repository } from "repository";

const repo = Repository.open(".");

// 1. Fetch recent commit log starting at HEAD
const entries = repo.log("HEAD", 10);
for (const entry of entries) {
  console.log(`${entry.id.slice(0, 7)} - ${entry.subject} (${entry.authorName})`);
}

// 2. Fetch first-parent history only (excluding branch merge commits)
const mainHistory = repo.log("main", 50, true);

// 3. Low-level history walking with revWalk
const tipId = repo.revParseSingle("HEAD");
const commitIds = repo.revWalk([tipId]);
for (const id of commitIds) {
  const commit = repo.findCommit(id);
  console.log(commit.id, commit.message);
}
```

### Verifying and Signing Commits

```javascript
import { Repository } from "repository";

const repo = Repository.open(".");
const commit = repo.headCommit();

// Verify signature
const verification = commit.verifySignature();
if (verification.signed) {
  console.log(`Signed by: ${verification.signer}, Valid: ${verification.valid}`);
} else {
  console.log("Commit is unsigned");
}

// Sign a commit (requires user.signingKey in git config)
const signedCommit = commit.sign();
console.log(`New signed commit ID: ${signedCommit.id}`);
```
