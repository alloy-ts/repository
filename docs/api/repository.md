# Repository API Reference

`repository` provides Node-API (NAPI-RS) bindings for the Rust [`gitoxide`](https://github.com/GitoxideLabs/gitoxide) (`gix`) crate, exporting `gix::Repository` capabilities to Node.js and TypeScript.

This document details the architecture, exported domain APIs, method signatures, and a complete mapping against `gix::Repository` (version 0.89.0).

---

## Table of Contents

1. [Architecture & Design Principles](#architecture--design-principles)
2. [Domain API Reference](#domain-api-reference)
   - [Repository Construction & Discovery](#1-repository-construction--discovery)
   - [Location & Path Accessors](#2-location--path-accessors)
   - [Identity & Configuration](#3-identity--configuration)
   - [References, Head & Namespace](#4-references-head--namespace)
   - [Objects & Database (ODB)](#5-objects--database-odb)
   - [Revision Parsing & History Traversal](#6-revision-parsing--history-traversal)
   - [Diff, Blame & Merge](#7-diff-blame--merge)
   - [Worktree, Index & Status](#8-worktree-index--status)
   - [Pathspec, Attributes & Excludes](#9-pathspec-attributes--excludes)
   - [Content Filter Pipeline](#10-content-filter-pipeline)
   - [Submodules](#11-submodules)
   - [Mailmap, Notes & Shallow Repositories](#12-mailmap-notes--shallow-repositories)
3. [Full `gix::Repository` Mapping Table](#full-gixrepository-mapping-table)

---

## Architecture & Design Principles

- **Thread Safety**: `Repository` wraps an `Arc<gix::ThreadSafeRepository>`. Synchronous calls obtain a thread-local handle via `self.inner.to_thread_local()`, ensuring memory efficiency and non-blocking multi-thread access in Node.js.
- **Asynchronous Execution**: Expensive IO and CPU tasks (such as `checkout`, `status`, `clone`, and `worktreeStream`) are offloaded to background libuv threads using NAPI `AsyncTask` Promises, keeping the Node.js event loop responsive.
- **Error Propagation**: Fallible operations map Rust `gix::Error` and domain errors into `napi::Error` exceptions with detailed diagnostic context messages.
- **Type Safety**: TypeScript definitions (`dist/index.d.ts`) are generated directly from Rust attributes (`#[napi]`, `#[napi(object)]`), providing compile-time type safety for inputs and return structures.

---

## Domain API Reference

### 1. Repository Construction & Discovery

#### `Repository.open(path: string): Repository`

Open an existing git repository at `path`.

#### `Repository.openOpts(path: string, bare?: boolean): Repository`

Open a repository with explicit bare/worktree overrides.

#### `Repository.init(path: string, bare?: boolean): Repository`

Initialize a new git repository at `path`.

#### `Repository.initOpts(path: string, bare?: boolean): Repository`

Initialize a repository with explicit options.

#### `Repository.discover(path?: string): Repository`

Discover a repository by searching upwards from `path` (defaults to current working directory).

#### `Repository.clone(url: string, path: string, bare?: boolean): Promise<Repository>`

Asynchronously clone a repository from `url` to local `path`.

---

### 2. Location & Path Accessors

#### `repo.gitDir(): string`

Return path to `.git` directory or bare repository root.

#### `repo.path(): string`

Synonym for `gitDir()`.

#### `repo.workDir(): string | null`

Return path to the working tree root, or `null` if bare.

#### `repo.commonDir(): string`

Return the common git directory (differs from `gitDir()` in linked worktrees).

#### `repo.isBare(): boolean`

Return `true` if this is a bare repository.

#### `repo.kind(): string`

Return repository kind string (e.g., `"Common"`).

#### `repo.objectHash(): string`

Return object hash algorithm name (`"Sha1"` or `"Sha256"`).

#### `repo.gitDirTrust(): string`

Return trust level (`"full"` or `"reduced"`).

#### `repo.currentDir(): string`

Return current working directory of the process.

#### `repo.installDir(): string`

Return install directory path.

#### `repo.prefix(): string | null`

Return repository relative prefix if running inside a subdirectory.

#### `repo.normalizePath(path: string): string`

Normalize path relative to repository root.

#### `repo.isPristine(): boolean | null`

Check if repository working tree is untouched.

#### `repo.workdirPath(relativePath: string): string | null`

Resolve path relative to working tree.

#### `repo.setWorkdir(workdir: string | null): string | null`

Set working tree path dynamically.

#### `repo.mainRepository(): Repository`

Return main repository handle if called on a linked worktree.

---

### 3. Identity & Configuration

#### `repo.defaultAuthor(): Signature | null`

Get default author identity from git configuration.

#### `repo.defaultCommitter(): Signature | null`

Get default committer identity from git configuration.

#### `repo.committerOrSetFallback(name: string, email: string): Signature`

Get committer or set explicit fallback identity.

#### `repo.committerOrSetGenericFallback(): Signature`

Get committer or set generic fallback identity (`gix` fallback).

#### `repo.configPath(): string | null`

Return path to local repository `.git/config` file.

#### `repo.configSnapshot(): Record<string, string>`

Return snapshot of resolved configuration key-value pairs.

#### `repo.bigFileThreshold(): number`

Return configured `core.bigFileThreshold` in bytes.

#### `repo.looseCompression(): number`

Return configured loose object compression level.

#### `repo.packCompression(): number`

Return configured packfile compression level.

#### `repo.diffAlgorithm(): string`

Return default diff algorithm (`"myers"`, `"histogram"`, etc.).

#### `repo.filesystemOptions(): FsCapabilities`

Return filesystem capabilities (`ignoreCase`, `executableBit`, `symlink`).

#### `repo.statOptions(): StatOptions`

Return file stat comparison configuration (`trustCtime`, `checkStat`, `useNsec`, `useStdev`).

---

### 4. References, Head & Namespace

#### `repo.head(): Head`

Return handle to `HEAD`.

#### `repo.headCommit(): Commit`

Return current commit pointed to by `HEAD`.

#### `repo.headId(): string`

Return 40-char hex object ID of `HEAD`.

#### `repo.headName(): string | null`

Return shorthand ref name of `HEAD` (e.g. `"main"`), or `null` if unborn.

#### `repo.headTree(): Tree`

Return root tree of current `HEAD` commit.

#### `repo.headTreeId(): string`

Return tree OID of current `HEAD` commit.

#### `repo.headTreeIdOrEmpty(): string`

Return tree OID of `HEAD` or empty tree OID if unborn.

#### `repo.headRef(): Reference | null`

Return underlying `Reference` for `HEAD` or `null` if detached.

#### `repo.setHead(target: string): void`

Update `HEAD` to point to ref name or commit OID.

#### `repo.references(): Reference[]`

List all references in the repository.

#### `repo.findReference(name: string): Reference`

Find reference by name (e.g., `"refs/heads/main"`).

#### `repo.tryFindReference(name: string): Reference | null`

Find reference by name, returning `null` if missing.

#### `repo.createReference(name: string, target: string, force?: boolean): Reference`

Create a new reference.

#### `repo.createTag(name: string, target: string, message?: string): Reference`

Create a tag reference (annotated if message provided).

#### `repo.createLightweightTag(name: string, target: string): Reference`

Create a lightweight tag ref.

#### `repo.tagReference(name: string): Reference | null`

Look up a tag ref by shorthand name.

#### `repo.editReference(edit: RefEditInput): string[]`

Execute reference edit transaction.

#### `repo.editReferences(edits: RefEditInput[]): string[]`

Execute multiple reference edit transactions atomically.

#### `repo.namespace(): string | null`

Get current ref namespace (e.g., `"refs/namespaces/foo/"`).

#### `repo.setNamespace(namespace: string): string | null`

Set active ref namespace.

#### `repo.clearNamespace(): string | null`

Clear active ref namespace.

---

### 5. Objects & Database (ODB)

#### `repo.findBlob(id: string): Blob`

Retrieve blob object by hex OID.

#### `repo.findTree(id: string): Tree`

Retrieve tree object by hex OID.

#### `repo.findCommit(id: string): Commit`

Retrieve commit object by hex OID.

#### `repo.findTag(id: string): Tag`

Retrieve tag object by hex OID.

#### `repo.findObject(id: string): GitObject`

Retrieve object wrapper by hex OID.

#### `repo.tryFindObject(id: string): GitObject | null`

Find object by OID, returning `null` if absent.

#### `repo.objectExists(id: string): boolean`

Check if object OID exists in database or packfiles.

#### `repo.findHeader(id: string): ObjectHeader`

Get object kind and uncompressed size header.

#### `repo.tryFindHeader(id: string): ObjectHeader | null`

Get object header or `null` if absent.

#### `repo.writeBlob(data: Buffer): string`

Write blob object bytes and return hex OID.

#### `repo.writeBlobStream(data: Buffer): string`

Stream blob bytes into object store.

#### `repo.writeTree(entries: TreeItemInput[]): string`

Write tree object from entries and return hex OID.

#### `repo.writeCommit(input: CommitInput): string`

Write commit object (tree, parents, message, author, committer) and return OID.

#### `repo.writeTag(input: TagInput): string`

Write annotated tag object and return OID.

#### `repo.writeObject(kind: string, data: Buffer): string`

Write raw object bytes for given kind (`"blob"`, `"tree"`, `"commit"`, `"tag"`).

#### `repo.editTree(baseTreeId: string, edits: TreeEditInput[]): string`

Apply edits to an existing tree and return new tree OID.

#### `repo.emptyTree(): Tree`

Return well-known empty tree object (`4b825dc642cb6eb9a060e54bf8d69288fbee4904`).

#### `repo.emptyBlob(): Blob`

Return well-known empty blob object (`e69de29bb2d1d6434b8b29ae775ad8c2e48c5391`).

#### `repo.peel(id: string, targetKind: string): string`

Peel object OID through tag/commit chain to target object kind (`"commit"`, `"tree"`, `"blob"`, `"tag"`).

#### `repo.objectCacheSize(bytes: number): void`

Set LRU object cache capacity in bytes.

#### `repo.objectCacheSizeIfUnset(bytes: number): void`

Set LRU object cache capacity if not already configured.

#### `repo.computeObjectCacheSizeForTreeDiffs(): number`

Calculate recommended object cache size for tree diff workloads.

#### `repo.withObjectMemory(): void`

Configure object store memory management.

---

### 6. Revision Parsing & History Traversal

#### `repo.revParseSingle(spec: string): string`

Resolve revision specifier (e.g. `"HEAD"`, `"main~2"`, `"v1.0"`) to hex OID.

#### `repo.revParseHex(spec: string): string`

Resolve revision specifier to hex OID string.

#### `repo.revParse(spec: string): RevSpec`

Parse complex revision specifier, returning kind (`"single"`, `"range"`, `"exclude"`) and OIDs.

#### `repo.revWalk(tips: string[], options?: WalkOptions): string[]`

Traverse commit history from tips with optional sorting, reversal, or first-parent filtering.

#### `repo.log(revision?: string, maxCount?: number, firstParent?: boolean): LogEntry[]`

Get commit history log summary entries.

#### `repo.isAncestor(ancestor: string, descendant: string): boolean`

Check if `ancestor` commit is reachable from `descendant`.

#### `repo.describe(id: string): string | null`

Describe commit OID using nearest tag name.

---

### 7. Diff, Blame & Merge

#### `repo.diffTreeToTree(oldId: string | null, newId: string): Change[]`

Diff two tree OIDs (or tree against empty) and return list of file changes (`Added`, `Modified`, `Deleted`, `Renamed`).

#### `repo.blame(filePath: string, commitId?: string): BlameOutcome`

Calculate line-by-line blame for file starting at `commitId` or `HEAD`.

#### `repo.mergeTrees(ourTree: string, theirTree: string, ancestorTree?: string): MergeOutcome`

Perform 3-way in-memory tree merge.

#### `repo.mergeCommits(ourCommit: string, theirCommit: string): MergeOutcome`

Perform 3-way commit merge with automatic virtual merge base resolution.

#### `repo.virtualMergeBase(one: string, two: string): string | null`

Calculate common ancestor or virtual merge base OID between two commits.

#### `repo.treeMergeOptions(): MergeTreeOptions`

Return tree merge configuration options.

#### `repo.blobMergeOptions(): MergeBlobOptions`

Return blob 3-way content merge options.

---

### 8. Worktree, Index & Status

#### `repo.status(): Promise<StatusEntry[]>`

Asynchronously inspect working tree status (modified, untracked, staged changes).

#### `repo.indexWorktree(): Promise<StatusEntry[]>`

Asynchronously scan index vs working tree changes.

#### `repo.checkout(target: string): Promise<string>`

Asynchronously check out commit or ref into working tree.

#### `repo.reset(target: string, hard?: boolean): void`

Reset current `HEAD` to target commit.

#### `repo.worktrees(): WorktreeProxy[]`

List linked worktrees.

#### `repo.worktreesIncludingMain(): Repository[]`

List all worktrees including main worktree as `Repository` instances.

#### `repo.worktreeProxyById(id: string): WorktreeProxy | null`

Find linked worktree proxy by ID.

#### `repo.worktree(): WorktreeInfo | null`

Return worktree metadata for current repository.

#### `repo.isWorktree(): boolean`

Return `true` if this repository handle represents a worktree.

#### `repo.worktreeStream(treeId: string): Promise<Buffer>`

Stream tar archive of tree contents.

#### `repo.addWorktree(id: string, path: string, refName?: string): WorktreeProxy`

Add a new linked worktree.

#### `repo.removeWorktree(id: string): void`

Remove a linked worktree.

---

### 9. Pathspec, Attributes & Excludes

#### `repo.pathspec(patterns: string[]): Pathspec`

Compile pathspec pattern matcher.

#### `repo.pathspecDefaults(): PathspecDefaults`

Return default pathspec configuration.

#### `repo.attributes(relativePath: string): AttributeEntry[]`

Query `.gitattributes` and `.gitignore` rules for path.

#### `repo.attributesOnly(relativePath: string): AttributeEntry[]`

Query `.gitattributes` rules only (without ignore checks).

#### `repo.isPathExcluded(relativePath: string): boolean`

Check if path is ignored by `.gitignore` rules.

---

### 10. Content Filter Pipeline

#### `FilterPipeline`

Wrapper around `gix::filter::Pipeline` for content conversion (EOL/CRLF, smudge/clean filter drivers).

```ts
const pipeline = new FilterPipeline(repo);
const toGit = pipeline.convertToGit(buffer, "file.txt");
const toWorktree = pipeline.convertToWorktree(buffer, "file.txt");
const obj = pipeline.worktreeFileToObject("file.txt");
```

---

### 11. Submodules

#### `repo.modules(): ModulesFile | null`

Get `.gitmodules` file handle.

#### `repo.submodules(): Submodule[]`

List all configured submodules.

#### `repo.submodule(name: string): Submodule | null`

Find submodule by name.

#### `repo.modulesPath(): string | null`

Return path to `.gitmodules` file.

---

### 12. Mailmap, Notes & Shallow Repositories

#### `repo.openMailmap(): Mailmap | null`

Load repository `.mailmap` file.

#### `repo.notes(refName?: string): NoteEntry[]`

List git notes under `refs/notes/commits` or custom ref.

#### `repo.isShallow(): boolean`

Check if repository is a shallow clone.

#### `repo.shallowFile(): string`

Return path to shallow commit file (`.git/shallow`).

#### `repo.shallowCommits(): string[] | null`

List OIDs of shallow boundary commits.

---

## Full `gix::Repository` Mapping Table

The table below maps all 172 public methods of Rust `gix::Repository` (version 0.89.0) against the Node-API JS/TS surface in `repository`.

| #   | `gix::Repository` Method                         | JS / TS Exported Method                         | Status      | Category & Notes                             |
| --- | ------------------------------------------------ | ----------------------------------------------- | ----------- | -------------------------------------------- |
| 1   | `add_worktree`                                   | `addWorktree(id, path, refName)`                | Implemented | Worktree creation                            |
| 2   | `attributes`                                     | `attributes(relativePath)`                      | Implemented | Pathattributes & ignores                     |
| 3   | `attributes_only`                                | `attributesOnly(relativePath)`                  | Implemented | `.gitattributes` only                        |
| 4   | `author`                                         | `defaultAuthor()`                               | Wrapped     | Mapped to default author                     |
| 5   | `big_file_threshold`                             | `bigFileThreshold()`                            | Implemented | Configuration                                |
| 6   | `blame_file`                                     | `blame(filePath, commitId)`                     | Wrapped     | Blame file history                           |
| 7   | `blob_merge_options`                             | `blobMergeOptions()`                            | Implemented | Merge options                                |
| 8   | `branch_names`                                   | `branchNames()`                                 | Implemented | Configured branches                          |
| 9   | `branch_remote`                                  | `branchRemote(name)`                            | Implemented | Branch remote lookup                         |
| 10  | `branch_remote_name`                             | `branchRemoteName(name)`                        | Implemented | Branch remote name                           |
| 11  | `branch_remote_ref_name`                         | `branchRemoteRefName(name)`                     | Implemented | Remote ref tracking                          |
| 12  | `branch_remote_tracking_ref_name`                | `branchRemoteTrackingRefName(name)`             | Implemented | Upstream ref tracking                        |
| 13  | `checkout_options`                               | N/A                                             | Internal    | Internal checkout builder                    |
| 14  | `clear_namespace`                                | `clearNamespace()`                              | Implemented | Ref namespace management                     |
| 15  | `command_context`                                | N/A                                             | Internal    | Command context plumbing                     |
| 16  | `commit`                                         | `findCommit(id)`                                | Implemented | Commit retrieval                             |
| 17  | `commit_as`                                      | `commitAs(...)`                                 | Implemented | Commit with explicit identity                |
| 18  | `commit_graph`                                   | `commitGraph()`                                 | Implemented | Commit-graph access                          |
| 19  | `commit_graph_if_enabled`                        | `commitGraphIfEnabled()`                        | Implemented | Conditional commit-graph                     |
| 20  | `commit_signing_options`                         | N/A                                             | Internal    | GPG signing options                          |
| 21  | `commit_signing_options_if_enabled`              | N/A                                             | Internal    | GPG signing check                            |
| 22  | `committer`                                      | `defaultCommitter()`                            | Wrapped     | Default committer identity                   |
| 23  | `committer_or_set_fallback`                      | `committerOrSetFallback(...)`                   | Implemented | Identity fallback                            |
| 24  | `committer_or_set_generic_fallback`              | `committerOrSetGenericFallback()`               | Implemented | Generic identity fallback                    |
| 25  | `common_dir`                                     | `commonDir()`                                   | Implemented | Common directory path                        |
| 26  | `compute_object_cache_size_for_tree_diffs`       | `computeObjectCacheSizeForTreeDiffs()`          | Implemented | Cache size calculation                       |
| 27  | `config_file_mut`                                | N/A                                             | Internal    | Mutable config file handle                   |
| 28  | `config_path`                                    | `configPath()`                                  | Implemented | Config file path                             |
| 29  | `config_snapshot`                                | `configSnapshot()`                              | Implemented | Config key-value map                         |
| 30  | `config_snapshot_mut`                            | N/A                                             | Internal    | Mutable config snapshot                      |
| 31  | `current_dir`                                    | `currentDir()`                                  | Implemented | Process directory                            |
| 32  | `delete_local_branches`                          | `deleteLocalBranches(...)`                      | Implemented | Branch deletion                              |
| 33  | `delete_local_branches_if_unchanged`             | `deleteLocalBranchesIfUnchanged(...)`           | Implemented | Safe branch deletion                         |
| 34  | `diff_algorithm`                                 | `diffAlgorithm()`                               | Implemented | Diff configuration                           |
| 35  | `diff_resource_cache`                            | N/A                                             | Internal    | Diff cache plumbing                          |
| 36  | `diff_resource_cache_for_tree_diff`              | N/A                                             | Internal    | Diff cache plumbing                          |
| 37  | `diff_tree_to_tree`                              | `diffTreeToTree(old, new)`                      | Implemented | Tree-to-tree diffing                         |
| 38  | `dirwalk`                                        | `dirwalk(...)`                                  | Implemented | Directory traversal                          |
| 39  | `dirwalk_iter`                                   | N/A                                             | Internal    | Dirwalk iterator                             |
| 40  | `dirwalk_options`                                | N/A                                             | Internal    | Dirwalk options builder                      |
| 41  | `editor`                                         | N/A                                             | Internal    | Git editor launcher                          |
| 42  | `editor_command`                                 | N/A                                             | Internal    | Git editor command                           |
| 43  | `edit_reference`                                 | `editReference(edit)`                           | Implemented | Single ref transaction                       |
| 44  | `edit_references`                                | `editReferences(edits)`                         | Implemented | Multi ref transaction                        |
| 45  | `edit_references_as`                             | `editReferencesAs(...)`                         | Implemented | Ref transaction with reflog identity         |
| 46  | `edit_tree`                                      | `editTree(base, edits)`                         | Implemented | Tree modification                            |
| 47  | `empty_blob`                                     | `emptyBlob()`                                   | Implemented | Well-known empty blob                        |
| 48  | `empty_reusable_buffer`                          | N/A                                             | Internal    | Buffer reuse plumbing                        |
| 49  | `empty_tree`                                     | `emptyTree()`                                   | Implemented | Well-known empty tree                        |
| 50  | `excludes`                                       | `attributes(path)`                              | Wrapped     | Exclude rules                                |
| 51  | `filesystem_options`                             | `filesystemOptions()`                           | Implemented | Filesystem capabilities                      |
| 52  | `filter_pipeline`                                | `FilterPipeline`                                | Wrapped     | Exposed via FilterPipeline class             |
| 53  | `find_blob`                                      | `findBlob(id)`                                  | Implemented | Blob lookup                                  |
| 54  | `find_commit`                                    | `findCommit(id)`                                | Implemented | Commit lookup                                |
| 55  | `find_default_remote`                            | `findDefaultRemote(...)`                        | Implemented | Default remote lookup                        |
| 56  | `find_fetch_remote`                              | `findFetchRemote(...)`                          | Implemented | Fetch remote lookup                          |
| 57  | `find_header`                                    | `findHeader(id)`                                | Implemented | Object header lookup                         |
| 58  | `find_object`                                    | `findObject(id)`                                | Implemented | Generic object lookup                        |
| 59  | `find_reference`                                 | `findReference(name)`                           | Implemented | Reference lookup                             |
| 60  | `find_remote`                                    | `findRemote(name)`                              | Implemented | Remote lookup                                |
| 61  | `find_tag`                                       | `findTag(id)`                                   | Implemented | Tag object lookup                            |
| 62  | `find_tree`                                      | `findTree(id)`                                  | Implemented | Tree lookup                                  |
| 63  | `git_dir`                                        | `gitDir()`                                      | Implemented | `.git` path                                  |
| 64  | `git_dir_trust`                                  | `gitDirTrust()`                                 | Implemented | Security trust level                         |
| 65  | `has_object`                                     | `objectExists(id)`                              | Wrapped     | Object existence check                       |
| 66  | `head`                                           | `head()`                                        | Implemented | HEAD accessor                                |
| 67  | `head_commit`                                    | `headCommit()`                                  | Implemented | HEAD commit accessor                         |
| 68  | `head_id`                                        | `headId()`                                      | Implemented | HEAD OID accessor                            |
| 69  | `head_name`                                      | `headName()`                                    | Implemented | HEAD ref name                                |
| 70  | `head_ref`                                       | `headRef()`                                     | Implemented | HEAD reference handle                        |
| 71  | `head_tree`                                      | `headTree()`                                    | Implemented | HEAD tree object                             |
| 72  | `head_tree_id`                                   | `headTreeId()`                                  | Implemented | HEAD tree OID                                |
| 73  | `head_tree_id_or_empty`                          | `headTreeIdOrEmpty()`                           | Implemented | HEAD tree or empty OID                       |
| 74  | `ignore_pattern_parser`                          | N/A                                             | Internal    | Ignore parser plumbing                       |
| 75  | `index`                                          | `index()`                                       | Implemented | Index file handle                            |
| 76  | `index_from_tree`                                | `indexFromTree(treeId)`                         | Implemented | Index creation from tree                     |
| 77  | `index_or_empty`                                 | `indexOrEmpty()`                                | Implemented | Index or empty fallback                      |
| 78  | `index_or_load_from_head`                        | `indexOrLoadFromHead()`                         | Implemented | Index from HEAD                              |
| 79  | `index_or_load_from_head_or_empty`               | N/A                                             | Internal    | Index resolution fallback                    |
| 80  | `index_path`                                     | `indexPath()`                                   | Implemented | `.git/index` path                            |
| 81  | `index_worktree_status`                          | `indexWorktree()`                               | Wrapped     | Index vs worktree status                     |
| 82  | `install_dir`                                    | `installDir()`                                  | Implemented | Installation directory                       |
| 83  | `into_sync`                                      | N/A                                             | Internal    | Rust ThreadSafeRepository conversion         |
| 84  | `is_bare`                                        | `isBare()`                                      | Implemented | Bare repository check                        |
| 85  | `is_dirty`                                       | `status()`                                      | Wrapped     | Dirty status check                           |
| 86  | `is_pristine`                                    | `isPristine()`                                  | Implemented | Pristine worktree check                      |
| 87  | `is_shallow`                                     | `isShallow()`                                   | Implemented | Shallow repository check                     |
| 88  | `kind`                                           | `kind()`                                        | Implemented | Repository kind                              |
| 89  | `loose_compression`                              | `looseCompression()`                            | Implemented | Compression setting                          |
| 90  | `main_repo`                                      | `mainRepository()`                              | Wrapped     | Main repository handle                       |
| 91  | `merge_base`                                     | `virtualMergeBase(...)`                         | Wrapped     | Merge base resolution                        |
| 92  | `merge_base_octopus`                             | `mergeBaseOctopus(...)`                         | Implemented | Multi-commit merge base                      |
| 93  | `merge_base_octopus_with_graph`                  | `mergeBaseOctopusWithGraph(...)`                | Implemented | Graph-assisted octopus merge base            |
| 94  | `merge_base_with_graph`                          | `mergeBaseWithGraph(...)`                       | Implemented | Graph-assisted merge base                    |
| 95  | `merge_bases_many`                               | `mergeBasesMany(...)`                           | Implemented | Multiple merge bases                         |
| 96  | `merge_bases_many_with_graph`                    | `mergeBasesManyWithGraph(...)`                  | Implemented | Graph-assisted merge bases                   |
| 97  | `merge_commits`                                  | `mergeCommits(our, their)`                      | Implemented | 3-way commit merge                           |
| 98  | `merge_resource_cache`                           | N/A                                             | Internal    | Merge cache plumbing                         |
| 99  | `merge_trees`                                    | `mergeTrees(our, their)`                        | Implemented | 3-way tree merge                             |
| 100 | `modules`                                        | `modules()`                                     | Implemented | `.gitmodules` file handle                    |
| 101 | `modules_path`                                   | `modulesPath()`                                 | Implemented | `.gitmodules` path                           |
| 102 | `namespace`                                      | `namespace()`                                   | Implemented | Ref namespace                                |
| 103 | `new_commit`                                     | `writeCommit(...)`                              | Wrapped     | Commit creation                              |
| 104 | `new_commit_as`                                  | `writeCommit(...)`                              | Wrapped     | Commit creation with identity                |
| 105 | `normalize_path`                                 | `normalizePath(path)`                           | Implemented | Path normalization                           |
| 106 | `notes`                                          | `notes(refName)`                                | Implemented | Git notes listing                            |
| 107 | `object_cache_size`                              | `objectCacheSize(bytes)`                        | Implemented | LRU cache capacity                           |
| 108 | `object_cache_size_if_unset`                     | `objectCacheSizeIfUnset(bytes)`                 | Implemented | Conditional cache capacity                   |
| 109 | `object_hash`                                    | `objectHash()`                                  | Implemented | Hash algorithm name                          |
| 110 | `open_index`                                     | `openIndex()`                                   | Implemented | Index file opening                           |
| 111 | `open_mailmap`                                   | `openMailmap()`                                 | Implemented | Mailmap file handle                          |
| 112 | `open_mailmap_into`                              | `openMailmapInto(...)`                          | Implemented | Mailmap loading                              |
| 113 | `open_modules_file`                              | `openModulesFile()`                             | Implemented | `.gitmodules` parser                         |
| 114 | `open_options`                                   | N/A                                             | Internal    | OpenOptions handle                           |
| 115 | `pack_compression`                               | `packCompression()`                             | Implemented | Pack compression level                       |
| 116 | `path`                                           | `path()`                                        | Implemented | Repository path                              |
| 117 | `pathspec`                                       | `pathspec(patterns)`                            | Implemented | Pathspec matcher                             |
| 118 | `pathspec_defaults`                              | `pathspecDefaults()`                            | Implemented | Pathspec options                             |
| 119 | `pathspec_defaults_inherit_ignore_case`          | `pathspecDefaultsInheritIgnoreCase(...)`        | Implemented | Pathspec ignore case options                 |
| 120 | `prefix`                                         | `prefix()`                                      | Implemented | Subdirectory prefix                          |
| 121 | `prepare_remove_worktree`                        | N/A                                             | Internal    | Worktree removal transaction                 |
| 122 | `reference`                                      | `findReference(name)`                           | Wrapped     | Reference resolution                         |
| 123 | `references`                                     | `references()`                                  | Implemented | All references list                          |
| 124 | `reload`                                         | N/A                                             | Internal    | Repository reload                            |
| 125 | `remote_at`                                      | `remoteAt(url)`                                 | Implemented | Anonymous remote at URL                      |
| 126 | `remote_at_without_url_rewrite`                  | `remoteAtWithoutUrlRewrite(url)`                | Implemented | Remote without rewrite                       |
| 127 | `remote_default_name`                            | `remoteDefaultName(...)`                        | Implemented | Default remote name                          |
| 128 | `remote_names`                                   | `remoteNames()`                                 | Implemented | Configured remote names                      |
| 129 | `remove_worktree`                                | `removeWorktree(id)`                            | Implemented | Worktree removal                             |
| 130 | `rev_parse`                                      | `revParse(spec)`                                | Implemented | Revision specifier parsing                   |
| 131 | `rev_parse_single`                               | `revParseSingle(spec)`                          | Implemented | Single revision resolution                   |
| 132 | `rev_walk`                                       | `revWalk(tips, options)`                        | Implemented | Commit history traversal                     |
| 133 | `revision_graph`                                 | N/A                                             | Internal    | Revision graph cache                         |
| 134 | `set_freelist`                                   | N/A                                             | Internal    | Rust memory freelist                         |
| 135 | `set_namespace`                                  | `setNamespace(ns)`                              | Implemented | Ref namespace setter                         |
| 136 | `set_workdir`                                    | `setWorkdir(path)`                              | Implemented | Working tree path setter                     |
| 137 | `shallow_commits`                                | `shallowCommits()`                              | Implemented | Shallow boundary OIDs                        |
| 138 | `shallow_file`                                   | `shallowFile()`                                 | Implemented | Shallow file path                            |
| 139 | `ssh_connect_options`                            | N/A                                             | Internal    | SSH connection options                       |
| 140 | `stat_options`                                   | `statOptions()`                                 | Implemented | Stat options                                 |
| 141 | `state`                                          | `state()`                                       | Implemented | In-progress state (rebase/merge/cherry-pick) |
| 142 | `status`                                         | `status()`                                      | Implemented | Worktree status                              |
| 143 | `submodules`                                     | `submodules()`                                  | Implemented | List submodules                              |
| 144 | `tag`                                            | `createTag(...)`                                | Wrapped     | Tag creation                                 |
| 145 | `tag_reference`                                  | `tagReference(name)`                            | Implemented | Tag reference lookup                         |
| 146 | `transport_options`                              | N/A                                             | Internal    | Transport options                            |
| 147 | `tree_index_status`                              | N/A                                             | Internal    | Tree vs index status                         |
| 148 | `tree_merge_options`                             | `treeMergeOptions()`                            | Implemented | Tree merge options                           |
| 149 | `try_find_header`                                | `tryFindHeader(id)`                             | Implemented | Optional header lookup                       |
| 150 | `try_find_object`                                | `tryFindObject(id)`                             | Implemented | Optional object lookup                       |
| 151 | `try_find_reference`                             | `tryFindReference(name)`                        | Implemented | Optional reference lookup                    |
| 152 | `try_find_remote`                                | `tryFindRemote(name)`                           | Implemented | Optional remote lookup                       |
| 153 | `try_find_remote_without_url_rewrite`            | `tryFindRemoteWithoutUrlRewrite(...)`           | Implemented | Remote lookup without rewrite                |
| 154 | `try_index`                                      | `tryIndex()`                                    | Implemented | Optional index handle                        |
| 155 | `try_into_repository_format_version`             | N/A                                             | Internal    | Format version validation                    |
| 156 | `upstream_branch_and_remote_for_tracking_branch` | `upstreamBranchAndRemoteForTrackingBranch(...)` | Implemented | Upstream tracking resolution                 |
| 157 | `virtual_merge_base`                             | `virtualMergeBase(one, two)`                    | Implemented | Virtual merge base OID                       |
| 158 | `virtual_merge_base_with_graph`                  | `virtualMergeBaseWithGraph(...)`                | Implemented | Graph-assisted virtual merge base            |
| 159 | `with_object_memory`                             | `withObjectMemory()`                            | Implemented | Memory management setup                      |
| 160 | `without_freelist`                               | N/A                                             | Internal    | Rust memory freelist                         |
| 161 | `work_dir`                                       | `workDir()`                                     | Implemented | Working tree path                            |
| 162 | `workdir`                                        | `workDir()`                                     | Wrapped     | Working tree path                            |
| 163 | `workdir_path`                                   | `workdirPath(path)`                             | Implemented | Relative workdir path                        |
| 164 | `worktree`                                       | `worktree()`                                    | Implemented | Worktree info                                |
| 165 | `worktree_archive`                               | `worktreeStream(treeId)`                        | Wrapped     | Worktree archive stream                      |
| 166 | `worktree_proxy_by_id`                           | `worktreeProxyById(id)`                         | Implemented | Worktree proxy lookup                        |
| 167 | `worktree_stream`                                | `worktreeStream(treeId)`                        | Implemented | Worktree archive stream                      |
| 168 | `worktrees`                                      | `worktrees()`                                   | Implemented | Linked worktrees list                        |
| 169 | `worktrees_including_main`                       | `worktreesIncludingMain()`                      | Implemented | Worktrees list including main                |
| 170 | `write_blob`                                     | `writeBlob(data)`                               | Implemented | Write blob object                            |
| 171 | `write_blob_stream`                              | `writeBlobStream(data)`                         | Implemented | Stream blob object                           |
| 172 | `write_object`                                   | `writeObject(kind, data)`                       | Implemented | Write raw object                             |
