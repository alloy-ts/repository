# Jujutsu (jj) Op-Heads-Store, Op-Store & Backend Storage NAPI Bindings

This document describes the Jujutsu (`jj-lib`) operation logging, operation heads store (`op-heads-store`), operation store (`op-store`), backend storage, caching `Store`, and repository bindings.

## Overview

In Jujutsu, repository state transitions are represented as an operation DAG (`Operation`), where each operation points to parent operations (`parentIds`) and records:

- An operation ID (`OperationId`)
- A view ID (`ViewId`) describing root commit heads, bookmarks, tags, and working copy checkouts
- Operation metadata (`OpMetadata`) including description, timestamp range, host, user, snapshot flag, and commit predecessors

The operation heads store (`OpHeadsStore`) tracks the current leaf heads of this operation DAG and handles concurrent multi-head resolution.

## Repository Integration

### Detection and Initialisation

- `repo.isOpRepo()` / `repo.hasOpStore()`: Returns `true` if `<git-dir>/ops`, configured `init.opsDir`, or `core.dbUrl` exists.
- `repo.dbUrl()`: Returns the effective SQLite database URL (`core.dbUrl` if configured, or default `file:<opsDir>/store.db`).
- `repo.initOpRepo()`: Initialises `<git-dir>/ops/op_store/`, `<git-dir>/ops/op_heads/`, and `<git-dir>/ops/store.db` under SQLite WAL mode.
- `repo.initOpRepoAt(opsDir)`: Initialises operation store files at the designated directory.
- `InitOptions.storeOp`: When set to `true` during `Repository.initOpts({ storeOp: true, opsDir?: string })`, initialises the repository with both a Git object store and an op store.
- `InitOptions.opsDir`: Customises operation store directory path, defaulting to `<git-dir>/ops` (persisted in config `init.opsDir`).

### Automatic Initialisation without `init`

If `core.dbUrl` is configured or `init.storeOp` is set to `true`, `repo.isOpRepo()` automatically checks and initializes `opsDir`, `op_heads`, and the SQLite database (`store.db`) under WAL mode, ensuring that op storage is immediately usable even when opening a standard repository without calling an explicit `init` method.

### SQLite WAL Mode as Default OpStore Backend

SQLite with WAL mode (`PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;`) is supported and configured as the default backend in `default_backend_factories()` and `default_op_store_initializer()`.

- **Atomic Operations & Views:** Operations and views commit together in SQLite transactions with zero torn writes.
- **Concurrent Access:** WAL mode allows concurrent readers to query head operations without locking or stalling writers.
- **Fast Append Writes:** Operations append sequentially to `<db>-wal` avoiding directory scan and fsync bottlenecks.

### Loose Object Store Cache Mode

When an op store is present:

- `repo.looseObjectStore()` returns a `LooseObjectStore` running in cache mode backed by the op store as the source of truth.
- Reading an object checks the op store / materializes objects from operation views.
- Writing an object writes directly to the op store and records state changes.

## Classes and Types

### `OpStore` & `OpStoreOpts`

```ts
interface OpStoreOpts {
  opsDir?: string;
  create?: boolean;
}

class OpStore {
  constructor(storePathOrOpts: string | OpStoreOpts, rootCommitId?: string);
  static open(storePathOrOpts: string | OpStoreOpts, rootCommitId?: string): OpStore;
  static init(storePathOrOpts: string | OpStoreOpts, rootCommitId?: string): OpStore;
}
```

### `Operation`

Represents an operation (transaction) on the repository view, maintaining a reference to its parent `OpStore`.

```ts
class Operation {
  constructor(
    id: string,
    viewId: string,
    parents: string[],
    description: string,
    metadata?: Record<string, string>,
    opStore?: OpStore,
  );

  id(): OperationId;
  viewId(): ViewId;
  parentIds(): OperationId[];
  parents(): Operation[];
  view(): View | null;
  metadata(): OpMetadata;
  description(): string;
  opStore(): OpStore | null;
  storesCommitPredecessors(): boolean;
  predecessorsForCommit(commitId: string): string[] | null;
  allReferencedCommitIds(): string[];
  storeOperation(): OperationRecord;
  toRecord(): OperationRecord;

  // Convert a single Commit object into an Operation, mapping any parent operations or parent commit IDs
  static from_commit(repo: Repository, commit: Commit, parentOpIds?: string[]): Operation;

  // Compose multiple sequential operations into a single merged operation
  static compose(operations: Operation[]): Operation;
}
```

### `SqlBackend` & `SimpleBackend`

```ts
class SqlBackend {
  static init(databaseUrl: string): SqlBackend;
  static load(databaseUrl: string): SqlBackend;
  name(): string;
  isWalMode(): boolean;
  commitIdLength(): number;
  changeIdLength(): number;
  rootCommitId(): string;
  rootChangeId(): string;
  emptyTreeId(): string;
  concurrency(): number;
  readFile(id: string): string;
  writeFile(contents: string): string;
  readSymlink(id: string): string;
  writeSymlink(target: string): string;
  readTree(id: string): string;
  writeTree(contents: string): string;
  readCommit(id: string): string;
  writeCommit(commitData: string): string;
}
```
