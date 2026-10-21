# Repository API Documentation

The `Repository` class is the primary entry point for working with Git repositories in Node.js via `gix` bindings.

---

## Table of Contents
- [Static Construction](#static-construction)
- [HEAD & Object Operations](#head--object-operations)
- [Index & Worktree Operations](#index--worktree-operations)
- [References & Remotes](#references--remotes)
- [Configuration & Inspection](#configuration--inspection)
- [Merge, Rebase & Revision Walk](#merge-rebase--revision-walk)

---

## Static Construction

### `Repository.open(path: string): Repository`
Opens an existing Git repository at `path`.

### `Repository.openOpts(path: string, bare?: boolean): Repository`
Opens a repository with extended options.

### `Repository.init(path: string, bare?: boolean): Repository`
Initializes a new Git repository at `path`.

### `Repository.discover(path?: string): Repository`
Discovers a Git repository by walking upwards from `path` (or current directory).

### `Repository.clone(url: string, path: string, bare?: boolean): Promise<Repository>`
Asynchronously clones a repository from `url` into `path`.

---

## HEAD & Object Operations

### `head(): Head`
Returns the `HEAD` reference descriptor.

### `headCommit(): Commit`
Returns the commit `HEAD` currently points to.

### `headId(): string`
Returns the object ID (hex string) `HEAD` points to.

### `headName(): string | null`
Returns the symbolic name of `HEAD` (e.g. `refs/heads/main`).

### `headTreeId(): string`
Returns the root tree ID of the HEAD commit.

### `findCommit(id: string): Commit`
Finds a commit by ID.

### `findTree(id: string): Tree`
Finds a tree by ID.

### `findBlob(id: string): Blob`
Finds a blob by ID.

### `findTag(id: string): Tag`
Finds an annotated tag by ID.

### `writeBlob(data: Buffer): string`
Writes raw buffer bytes as a blob object and returns its object ID.

### `writeCommit(input: CommitInput): string`
Writes a loose commit object directly into the ODB.

### `writeTag(input: TagInput): string`
Writes an annotated tag object into the ODB.

### `peel(id: string, kind: string): string`
Peels a tag/commit object down to the target kind (`commit`, `tree`, `blob`, `tag`).

---

## Index & Worktree Operations

### `indexEntries(): IndexEntry[]`
Reads all entries currently stored in the index.

### `editIndex(entries: IndexEntryInput[]): void`
Mutates and persists entries in the repository index file.

### `status(): Promise<StatusEntry[]>`
Computes working tree status asynchronously.

### `checkout(target: string): Promise<string>`
Checks out a target commit asynchronously into the worktree.

### `reset(target: string, hard?: boolean): void`
Resets `HEAD`, index, and optionally working tree to `target`.

### `worktrees(): WorktreeProxy[]`
Lists secondary worktrees for this repository.

### `mainWorktree(): WorktreeProxy | null`
Returns descriptor for the main worktree.

### `isWorktree(): boolean`
Returns `true` if this instance is a linked secondary worktree.

---

## References & Remotes

### `references(): Reference[]`
Lists all references in the repository.

### `findReference(name: string): Reference`
Finds a reference by name (e.g. `refs/heads/main`, `HEAD`).

### `createReference(name: string, target: string, force?: boolean): Reference`
Creates or updates a reference.

### `remotes(): Remote[]`
Lists all named remotes.

### `findRemote(name: string): Remote`
Finds a remote by name.

### `findDefaultRemote(): Remote | null`
Finds the default remote for fetching/pushing.

---

## Configuration & Inspection

### `configString(key: string): string | null`
Reads a single configuration value by key.

### `configEntries(): ConfigEntry[]`
Lists all flattened configuration key/value entries.

### `configSet(key: string, value: string): void`
Sets a configuration key in local config.

### `configPath(source?: string): string | null`
Returns the filesystem path of the configuration file for a source (`local`, `global`, `system`).

### `isShallow(): boolean`
Returns `true` if the repository is a shallow clone.

### `shallowCommits(): string[]`
Returns the IDs of shallow boundary commits.

### `state(): string`
Returns the current operation state (e.g. `bare`, `clean`, `rebase`, `merge`).

---

## Merge, Rebase & Revision Walk

### `revParseSingle(spec: string): string`
Resolves a revision specifier (e.g. `HEAD~1`, `main`) to an object ID.

### `revWalk(tips: string[]): string[]`
Walks commits starting from the given tips in reverse chronological order.

### `log(rev?: string, limit?: number, firstParent?: boolean): LogEntry[]`
Returns commit history summaries starting at `rev`.

### `mergeTrees(ourTree: string, theirTree: string, ancestorTree: string): MergeOutcome`
Merges two trees using a common ancestor tree.

### `mergeCommits(ourCommit: string, theirCommit: string): MergeOutcome`
Merges two commits and returns the resulting merged tree ID and conflict status.
