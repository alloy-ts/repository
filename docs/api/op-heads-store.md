# OpHeadsStore API (Jujutsu / jj Integration)

The `OpHeadsStore` manages the operation heads and operation history DAG of a
Jujutsu repository (`.jj/repo/op_heads/` and `.jj/repo/op_store/`), mirroring
`jj_lib::op_heads_store`, `SimpleOpHeadsStore`, and `jj_lib::repo::Repo`.

In Jujutsu (jj), every change made to the repository (commits, branch movements,
rebases, checkouts) is recorded as an immutable **Operation** pointing to a
repository **View**. The set of current tips of the operation log DAG are
tracked by the **OpHeadsStore**.

## Architecture & Layout

A Jujutsu repository stores operation heads and operation records on disk under:

- `.jj/repo/op_heads/type`: backend type string (`"simple_op_heads_store"`).
- `.jj/repo/op_heads/heads/<op_id>`: empty files whose filenames are operation IDs.
- `.jj/repo/op_heads/heads/lock`: exclusive file lock preventing concurrent divergence resolution.
- `.jj/repo/op_store/type`: backend type string (`"simple_op_store"`).
- `.jj/repo/op_store/operations/<op_id>`: operation record (view ID, parent operation IDs, metadata).
- `.jj/repo/op_store/views/<view_id>`: repository view (head commit IDs, working-copy commits, bookmarks, tags, git refs).

## Classes & Interfaces

### `OpHeadsStore`

Primary handle to the operation heads and operation log store.

#### Constructor & Static Initializer

| Signature                                                                    | Notes                                                                                            |
| ---------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| `new OpHeadsStore(repo_dir: string)`                                         | Opens an existing op-heads-store at `repo_dir` (e.g. `.jj/repo`).                                |
| `OpHeadsStore.init(repo_dir: string, root_commit_id?: string): OpHeadsStore` | Creates base directories, initialises root view, root operation, and registers the initial head. |

#### Methods

| Method           | Signature                                     | Notes                                                                                                                 |
| ---------------- | --------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| `getOpHeads`     | `() => string[]`                              | Returns all current operation head IDs.                                                                               |
| `addOpHead`      | `(op_id: string) => void`                     | Adds an operation head ID.                                                                                            |
| `removeOpHead`   | `(op_id: string) => void`                     | Removes an operation head ID.                                                                                         |
| `updateOpHeads`  | `(old_ids: string[], new_id: string) => void` | Atomically replaces `old_ids` with `new_id`.                                                                          |
| `lock`           | `() => OpHeadsLock`                           | Acquires an exclusive file lock (`.jj/repo/op_heads/heads/lock`).                                                     |
| `writeOperation` | `(operation: OperationRecord) => string`      | Writes an operation record and returns its ID.                                                                        |
| `readOperation`  | `(op_id: string) => OperationRecord`          | Reads an operation record by ID.                                                                                      |
| `writeView`      | `(view: ViewRecord) => string`                | Writes a repository view record and returns its ID.                                                                   |
| `readView`       | `(view_id: string) => ViewRecord`             | Reads a repository view record by ID.                                                                                 |
| `resolveOpHeads` | `() => ResolvedOpHeads`                       | Resolves current operation heads down to a single head under a lock, merging divergent views if multiple heads exist. |

### `Repository` Extensions

The `Repository` class in `src/repo/mod.rs` exposes direct accessors for Jujutsu repositories:

| Method                  | Signature            | Notes                                                                         |
| ----------------------- | -------------------- | ----------------------------------------------------------------------------- |
| `repo.isJjRepo()`       | `() => boolean`      | Checks if a `.jj` directory exists in the working directory or git directory. |
| `repo.jjOpHeadsStore()` | `() => OpHeadsStore` | Returns an `OpHeadsStore` bound to `.jj/repo`.                                |
| `repo.initJj()`         | `() => OpHeadsStore` | Initializes Jujutsu repository layout and returns its `OpHeadsStore`.         |

### Types

```ts
interface OpTimestampRange {
  startMillis: bigint;
  endMillis: bigint;
  tzOffset: number;
}

interface OpMetadata {
  description: string;
  hostname: string;
  username: string;
  isSnapshot: boolean;
  workspaceName?: string;
  time: OpTimestampRange;
  attributes: Record<string, string>;
}

interface OperationRecord {
  id: string;
  viewId: string;
  parents: string[];
  metadata: OpMetadata;
}

interface OpRefTarget {
  adds: string[];
  removes: string[];
  isConflict: boolean;
}

interface ViewRecord {
  id: string;
  headIds: string[];
  wcCommitIds: Record<string, string>;
  localBookmarks: Record<string, OpRefTarget>;
  localTags: Record<string, OpRefTarget>;
  gitRefs: Record<string, OpRefTarget>;
  gitHeads: Record<string, OpRefTarget>;
}

interface ResolvedOpHeads {
  opId: string;
  heads: string[];
  wasDivergent: boolean;
}
```

## Example Usage

```ts
import { Repository, OpHeadsStore } from "@alloy-ts/repository";

// 1. Initialize jj metadata alongside a Git repository
const repo = Repository.open(".");
if (!repo.isJjRepo()) {
  repo.initJj();
}

const store = repo.jjOpHeadsStore();
console.log("Current op heads:", store.getOpHeads());

// 2. Resolve divergent operation heads automatically
const resolved = store.resolveOpHeads();
if (resolved.wasDivergent) {
  console.log("Reconciled divergent operations to:", resolved.opId);
}
```
