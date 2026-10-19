# `gix` Crate & `Repository` API Overview

This document reviews the API exported by the [`gix`](https://docs.rs/gix) Rust crate (gitoxide) and documents how it is surfaced to JavaScript/TypeScript via NAPI-RS in this repository (`repository`).

---

## 1. `gix` Crate Overview & Architecture

`gix` is the central high-level crate of the **gitoxide** project, providing idiomatic, high-performance, and thread-safe access to Git repositories.

### Key Handles

- **`gix::ThreadSafeRepository`**: An `Arc`-shareable, thread-safe handle to a repository object database, reference database, and configuration. Ideal for async tasks, background libuv threads, or worker thread pools.
- **`gix::Repository`**: A thread-local handle obtained from `ThreadSafeRepository::to_thread_local()`. It caches object lookups, holds snapshot views of configuration, and provides high-level domain methods.

### Primary Functions & Module Structure

- **Initialization & Discovery**:
  - `gix::open(path)` / `gix::ThreadSafeRepository::open(path)`
  - `gix::init(path)` / `gix::init_bare(path)`
  - `gix::discover(path)`
  - `gix::prepare_clone(url, path)` / `gix::prepare_clone_bare(url, path)`
- **Objects (`gix::object`, `gix::objs`)**:
  - `gix::Commit`: Decodes author, committer, tree, parent ids, and signature information.
  - `gix::Tree`: Iterates over directory entries with filename, mode, and target OID.
  - `gix::Blob`: Provides direct byte access to file contents.
  - `gix::Tag`: Decodes annotated tags (tagger, target OID, target kind, message).
- **References (`gix::refs`, `gix::Head`)**:
  - `gix::Head`: Inspects symbolic referents, peeled object IDs, or HEAD commit objects.
  - `gix::Reference`: Represents branch, tag, or custom refs with peeling and deletion support.
- **Remotes (`gix::remote`)**:
  - `gix::Remote`: Named or anonymous remotes configured with fetch/push URLs and refspecs. Supports network transfer connections (`connect`, `prepare_fetch`, `receive`).
- **Revisions & History (`gix::revision`)**:
  - `rev_parse_single(spec)` / `rev_walk(tips)`: Graph traversal and rev-parsing.
  - `merge_base(one, two)` / `merge_base_octopus(commits)`: Finding common ancestor commits.
- **Commit Signing & Verification (`gix::commit::verify`, `gix::commit::sign`)**:
  - `verify_signature()`: Checks cryptographic signatures using GPG, SSH, or x509 programs.
  - `sign()`: Signs commit payloads using configured `user.signingKey` and program binaries.

---

## 2. JavaScript / TypeScript Binding (`repository`)

The `repository` package wraps `Arc<gix::ThreadSafeRepository>` inside a TypeScript `Repository` class exposed via NAPI-RS.

### Exported Classes and Interfaces

```ts
import {
  Repository,
  Commit,
  Tree,
  Blob,
  Tag,
  GitObject,
  Reference,
  Head,
  Remote,
  type Signature,
  type SignatureVerification,
  type LogEntry,
  type Change,
  type RemoteFetchOutcome,
} from "repository";
```

#### Static Constructors (`Repository`)

- `Repository.open(path: string): Repository`
- `Repository.init(path: string, bare?: boolean): Repository`
- `Repository.discover(path?: string): Repository`
- `Repository.clone(url: string, path: string, bare?: boolean): Promise<Repository>`

#### Core Methods on `Repository`

- **HEAD & Resolution**: `head()`, `headCommit()`, `headId()`, `headName()`
- **Object Lookup & Creation**: `findCommit(id)`, `findTree(id)`, `findBlob(id)`, `findTag(id)`, `findObject(id)`, `objectExists(id)`, `writeBlob(data)`
- **References & Tags**: `references()`, `findReference(name)`, `createReference(name, target, force)`, `createTag(name, target, message, force)`, `createLightweightTag(name, target, force)`
- **Remotes**: `remotes()`, `findRemote(name)`
- **Revisions & History**: `revParseSingle(spec)`, `revParse(spec)`, `revWalk(tips)`, `mergeBase(one, two)`, `mergeBaseOctopus(commits)`, `log(rev?, limit?, firstParent?)`
- **Diffs**: `diffTreeToTree(oldId?, newId)`
- **Configuration & Metadata**: `configString(key)`, `workDir()`, `gitDir()`, `commonDir()`, `isBare()`, `kind()`, `objectHash()`

---

## 3. Reading Git History (Log) in JavaScript

`Repository.prototype.log` returns commit summaries in reverse-chronological order without requiring manual per-commit async round-trips across NAPI boundaries.

```ts
import { Repository } from "repository";

const repo = Repository.open(".");

// Fetch recent history starting at HEAD (limit to 10 commits)
const history = repo.log("HEAD", 10);

for (const entry of history) {
  console.log(`Commit: ${entry.id}`);
  console.log(`Author: ${entry.authorName} <${entry.authorEmail}>`);
  console.log(`Date:   ${new Date(entry.authorTime * 1000).toISOString()}`);
  console.log(`Subject: ${entry.subject}\n`);
}
```
