# API Documentation

Welcome to the `@alloy-ts/repository` API documentation. The repository provides high-speed Git operations, cryptographic key vaults, object databases, SQL schema compiling, background task scheduling, and language runtimes via N-API.

## Documentation Sections

### 1. [Git Repository API](./repo/index.md) (`src/repo/`)

Core Git and Gitoxide (`gix`) bindings:

- **[Index](./repo/index.md)**: Staging area access, stat checking, entries, and persistence.
- **[Commit](./repo/commit.md)**: Commits, parentage, trees, authors, and commit graph queries.
- **[Tree](./repo/tree.md)**: Tree entries, traversal, Prolly trees, and tree editing.
- **[Diff](./repo/diff.md)**: Myers / Histogram line diffs and tree-to-tree comparisons.
- **[Blame](./repo/blame.md)**: Line-by-line blame attribution and commit metadata.
- **[Status](./repo/status.md)**: Working copy status, porcelain checks, and modified files.
- **[Object](./repo/object.md)**: OIDs, blobs, trees, tags, and commits.
- **[Path](./repo/path.md)**: Repository path utilities and normalization.
- **[Progress](./repo/progress.md)**: Event-driven transfer progress reporting.
- **[Reflog](./repo/reflog.md)**: Reference log inspection and history tracking.
- **[Refspec](./repo/refspec.md)**: Push and fetch refspec mapping and validation.
- **[Rev Walk](./repo/rev_walk.md)**: Configurable commit history traversal.
- **[Revision](./repo/revision.md)**: Revspec parsing (`HEAD~2`, branches, tags) and peeling.
- **[Tag](./repo/tag.md)**: Annotated and lightweight tag handling.
- **[Trailer](./repo/trailer.md)**: Commit message trailers (`Signed-off-by:`, `Co-authored-by:`).
- **[Attributes](./repo/attributes.md)**: `.gitattributes` query and pattern checking.
- **[Describe](./repo/describe.md)**: Human-readable commit naming (`git describe`).

### 2. [Cryptography & Vault API](./crypto/index.md) (`src/crypto/`)

- **[Crypto](./crypto/index.md)**: Symmetric encryption (`SymmetricCryptoKey`), git-crypt filters, and git-remote-gcrypt entire repository encryption.
- **[OpenPGP / GPG](./crypto/index.md#gpg--openpgp-subsystem-srccryptogpgrs)**: OpenPGP signing, verification, encryption, and system keyring lookup via `rPGP`.
- **[Vault & Secret Storage](./crypto/vault.md)**: Encrypted password-protected vaults (`Vault`), local KV store (`KvVault`), and repository key managers (`KeyManager`).

### 3. [Object Store API](./object_store/index.md) (`src/object_store/`)

- **[Object Stores](./object_store/index.md)**: Repository-independent object databases, loose stores (`ObjectStore`), and pack-aware cached databases (`ObjectDatabase`).
- **[Op-Heads & OpStore](./object_store/op-heads-store.md)**: Jujutsu (`jj`) operation DAG logging, views, operations, and SQLite WAL storage.

### 4. [SQL Compiler & QueryDSL](./sql/index.md) (`src/sql/`)

- **[Schema Compiler](./sql/index.md)**: Intermediate Representation (IR), Diesel tables, Rust models, NAPI-RS classes, and migration generator.
- **[QueryDSL & RunQueryDsl](./sql/index.md#sql-querydsl--runquerydsl-srcsqlqueryrs)**: Type-safe, chainable query builder and SQLite connection executor.

### 5. [Task & Threading API](./task/index.md) (`src/task/`)

- **[Task](./task/index.md)**: Worker thread execution, message passing (`postMessage`), and event loops.
- **[Process](./task/process.md)**: Async child processes, command execution, and cancellation tokens.
- **[Thread](./task/thread.md)**: OS threads (`Thread`, `Builder`, `JoinHandle`) and libuv thread pool inspection.
- **[Scheduler](./task/scheduler.md)**: Task scheduling, dispatching, and queue handles.
- **[Promise](./task/promise.md)**: Asynchronous promise bridging with Rust worker pools.

### 6. [Engine Language Runtime](./engine/index.md) (`src/engine/`)

- **[Engine & FuseVM](./engine/index.md)**: Bytecode compiler and virtual machine runtime for the custom Engine programming language.
