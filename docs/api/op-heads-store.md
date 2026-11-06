# Jujutsu (jj) Op-Heads-Store & Op-Store NAPI Bindings

This document describes the Jujutsu (`jj-lib`) operation logging, operation heads store (`op-heads-store`), operation store (`op-store`), backend storage, and repository bindings.

## Overview

In Jujutsu, operation history represents transactional repository changes as a directed acyclic graph (DAG) of operations (`OperationRecord`), where each operation points to parent operations and records:

- An operation ID (`OperationId`)
- A view ID (`ViewId`) describing root commit heads, bookmarks, tags, and working copy checkouts
- Operation metadata (`OpMetadata`) including description, timestamp range, host, user, and snapshot flag

The operation heads store (`OpHeadsStore`) tracks the current leaf heads of this operation DAG and handles concurrent multi-head resolution.

## Repository Integration

### Detection and Initialisation

- `repo.isOpRepo()` / `repo.hasOpStore()`: Returns `true` if `.git/repo/op_store/` or `.jj/` is present.
- `repo.initOpRepo()`: Initialises `.git/repo/op_store/` and `.git/repo/op_heads/` with a root view and operation.
- `init.storeOp`: When set to `true` during `Repository.initOpts({ storeOp: true })`, the repository is initialised with both a Git object store and an op store.

### Loose Object Store Cache Mode

When an op store is present:

- `repo.looseObjectStore()` returns a `LooseObjectStore` running in cache mode backed by the op store as the source of truth.
- Reading an object checks the op store / materializes objects from operation views.
- Writing an object writes directly to the op store and records state changes.

## Classes and Types

### `Op`

Represents an operation in the operation graph.

```ts
class Op {
  constructor(
    id: string,
    viewId: string,
    parents: string[],
    description: string,
    metadata?: Record<string, string>,
  );

  id(): string;
  viewId(): string;
  parents(): string[];
  description(): string;
  metadata(): Record<string, string>;
  toRecord(): OperationRecord;

  // Convert Git commits to operations
  static fromCommits(repo: Repository, commitIds: string[]): Op[];
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

### `SimpleBackend`

```ts
const COMMIT_ID_LENGTH: number = 64;
const CHANGE_ID_LENGTH: number = 16;

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
