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

- `repo.isOpRepo()` / `repo.hasOpStore()`: Returns `true` if `<git-dir>/ops` or configured `init.opsDir` exists.
- `repo.initOpRepo()`: Initialises `<git-dir>/ops/op_store/` and `<git-dir>/ops/op_heads/` with a root view and operation.
- `repo.initOpRepoAt(opsDir)`: Initialises operation store files at the designated directory.
- `InitOptions.storeOp`: When set to `true` during `Repository.initOpts({ storeOp: true, opsDir?: string })`, initialises the repository with both a Git object store and an op store.
- `InitOptions.opsDir`: Customises operation store directory path, defaulting to `<git-dir>/ops` (persisted in config `init.opsDir`).

### Loose Object Store Cache Mode

When an op store is present:

- `repo.looseObjectStore()` returns a `LooseObjectStore` running in cache mode backed by the op store as the source of truth.
- Reading an object checks the op store / materializes objects from operation views.
- Writing an object writes directly to the op store and records state changes.

## Classes and Types

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

### `ViewId` & `OperationId`

```ts
class ViewId {
  constructor(hex: string);
  hex(): string;
}

class OperationId {
  constructor(hex: string);
  hex(): string;
}
```

### `RefTarget`, `RemoteRef`, `RemoteView`, and `View`

```ts
interface RefTarget {
  commitId?: string;
  conflictCommits?: string[];
}

enum RemoteRefState {
  New = 0,
  Tracked = 1,
}

class RemoteRef {
  constructor(target: RefTarget, state: RemoteRefState);
  static absent(): RemoteRef;
  isAbsent(): boolean;
  isPresent(): boolean;
  isTracked(): boolean;
  target(): RefTarget;
  state(): RemoteRefState;
}

class View {
  constructor(headIds: string[]);
  static makeRoot(rootCommitId: string): View;
  headIds(): string[];
  getLocalBookmark(name: string): RefTarget | null;
  setLocalBookmark(name: string, target: RefTarget): void;
  getWcCommitId(workspaceName: string): string | null;
  setWcCommitId(workspaceName: string, commitId: string): void;
}
```

### `SimpleBackend` & `Store`

```ts
const COMMIT_ID_LENGTH: number = 64;
const CHANGE_ID_LENGTH: number = 16;
const COMMIT_CACHE_CAPACITY: number = 100;
const TREE_CACHE_CAPACITY: number = 1000;

class SimpleBackend {
  static init(storePath: string): SimpleBackend;
  static load(storePath: string): SimpleBackend;
  name(): string;
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

class Store {
  constructor(backend: SimpleBackend);
  static init(storePath: string): Store;
  commitIdLength(): number;
  changeIdLength(): number;
  rootCommitId(): string;
  rootChangeId(): string;
  emptyTreeId(): string;
  concurrency(): number;
  getCommit(id: string): string;
  writeCommit(commitData: string): string;
  getTree(id: string): string;
  writeTree(contents: string): string;
  readFile(id: string): string;
  writeFile(contents: string): string;
  readSymlink(id: string): string;
  writeSymlink(contents: string): string;
  clearCaches(): void;
}
```

### `Workspace` & `WorkspaceLoader`

```ts
class Workspace {
  static initSimple(workspaceRoot: string, workspaceName?: string): Workspace;
  static initWorkspaceWithExistingRepo(
    workspaceRoot: string,
    repoPath: string,
    workspaceName?: string,
  ): Workspace;
  static load(workspaceRoot: string): Workspace;

  workspaceRoot(): string;
  workspaceName(): string;
  repoPath(): string;
  repo(): Repository;
  startWorkingCopyMutation(): LockedWorkspace;
  checkOut(operationId: string, commitId?: string): CheckoutStats;
}
```
