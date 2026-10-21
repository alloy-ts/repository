# Repository API Reference (`@yumin-chen/repository` / `gix`)

The `Repository` class is the central handle for interacting with a Git repository using `gix` (gitoxide) v0.89 native bindings.

---

## Overview & Architecture

`Repository` wraps `Arc<gix::ThreadSafeRepository>`.
- **Synchronous methods** acquire a cheap thread-local handle (`thread_local(&self.inner)`) for fast, non-blocking reads and local mutations.
- **Asynchronous / Blocking methods** (e.g., `clone`, `status`, `checkout`) run on the libuv thread pool using `AsyncTask` so they never block the Node.js event loop.

---

## Full API Surface

### 1. Static Constructors & Discovery
- `Repository.open(path: string): Repository`
  Open a repository at `path` (or `path/.git`).
- `Repository.openOpts(path: string, bare?: boolean): Repository`
  Open a repository with custom options.
- `Repository.init(path: string, bare?: boolean): Repository`
  Initialize a new Git repository at `path`.
- `Repository.initOpts(path: string, bare?: boolean): Repository`
  Initialize a repository with options.
- `Repository.discover(path?: string): Repository`
  Discover a repository by searching upwards from `path` (or cwd).
- `Repository.clone(url: string, path: string, bare?: boolean): Promise<Repository>`
  Asynchronously clone a remote or local repository to `path`.

---

### 2. HEAD & Object Database (ODB)
- `head(): Head` — Retrieve the current HEAD handle.
- `headCommit(): Commit` — Retrieve the commit HEAD currently points to.
- `headId(): string` — Object ID (40-hex OID) HEAD points to.
- `headName(): string | null` — Symbolic referent name of HEAD (e.g., `refs/heads/main`).
- `headTreeId(): string` — Root tree OID pointed to by HEAD.
- `headRef(): Reference | null` — Reference HEAD points to, or null if detached.
- `headTree(): Tree` — Root tree object pointed to by HEAD.
- `setHead(target: string): void` — Point HEAD to an object ID.
- `findCommit(id: string): Commit` — Decode commit by ID.
- `findTree(id: string): Tree` — Decode tree by ID.
- `findBlob(id: string): Blob` — Retrieve blob handle by ID.
- `findTag(id: string): Tag` — Decode annotated tag by ID.
- `findObject(id: string): GitObject` — Find any object, returning a wrapper with `asCommit()`, `asTree()`, `asBlob()`, `asTag()`.
- `tryFindObject(id: string): GitObject | null` — Safe lookup returning null if missing.
- `objectExists(id: string): boolean` — Check whether an OID exists in the ODB.
- `writeBlob(data: Buffer): string` — Write raw bytes as a blob, returning OID.
- `writeBlobStream(data: Buffer): string` — Write a blob via streaming ODB path.
- `writeCommit(input: CommitInput): string` — Write a loose commit object without moving refs.
- `writeTag(input: TagInput): string` — Write an annotated tag object directly to ODB.
- `writeTree(entries: TreeItemInput[]): string` — Write a tree object from entry definitions.
- `writeObject(kind: string, data: Buffer): string` — Write a raw object of kind (`commit`, `tree`, `blob`, `tag`).
- `editTree(baseTreeId: string, edits: TreeEditInput[]): string` — Modify tree entries and produce a new tree OID.
- `peel(id: string, kind: string): string` — Peel tag/commit object down to specified kind.
- `emptyTree(): Tree` — Return the canonical empty tree object.
- `emptyBlob(): Blob` — Return the canonical empty blob object.
- `findHeader(id: string): ObjectHeader` — Read object kind and size without full decoding.
- `tryFindHeader(id: string): ObjectHeader | null` — Safe header lookup returning null if missing.

---

### 3. References & Namespaces
- `references(): Reference[]` — List all references in repository.
- `findReference(name: string): Reference` — Look up ref by name (e.g., `HEAD`, `refs/heads/main`).
- `tryFindReference(name: string): Reference | null` — Safe lookup returning null if missing.
- `createReference(name: string, target: string, force?: boolean): Reference` — Create or overwrite reference.
- `createTag(name: string, target: string, message: string, force?: boolean): Reference` — Create annotated tag ref.
- `createLightweightTag(name: string, target: string, force?: boolean): Reference` — Create lightweight tag ref.
- `tagReference(name: string, target: string, force?: boolean): Reference` — Create lightweight tag ref.
- `namespace(): string | null` — Retrieve active reference namespace.
- `setNamespace(ns: string): string | null` — Set reference namespace (e.g. `feature-x`).
- `clearNamespace(): string | null` — Clear active reference namespace.
- `editReference(edit: RefEditInput): string[]` — Apply reference edit transaction.
- `editReferences(edits: RefEditInput[]): string[]` — Apply multiple reference edit transactions.
- `editReferencesAs(edits: RefEditInput[], committer?: Signature): string[]` — Edit references with custom committer identity.

---

### 4. Remotes
- `remotes(): Remote[]` — List all named remotes.
- `findRemote(name: string): Remote` — Find remote by name (e.g., `origin`).
- `tryFindRemote(name: string): Remote | null` — Safe lookup for named remote.
- `tryFindRemoteWithoutUrlRewrite(name: string): Remote | null` — Lookup without applying URL rewrites.
- `findDefaultRemote(direction: RemoteDirection): Remote | null` — Configured default remote for fetch/push.
- `findFetchRemote(nameOrUrl?: string): Remote` — Resolve fetch remote by name or URL.
- `remoteAt(url: string): Remote` — Construct anonymous remote handle for URL.
- `remoteAtWithoutUrlRewrite(url: string): Remote` — Construct anonymous remote handle without rewrite rules.
- `createRemote(name: string, url: string): Remote` — Create and persist named remote to config.

---

### 5. Configuration & Paths
- `configString(key: string): string | null` — Read single config value as string.
- `configEntries(): ConfigEntry[]` — Retrieve all flattened configuration key/value entries.
- `configSet(key: string, value: string): void` — Set configuration key in local config file.
- `branchNames(): string[]` — List configured local branch names.
- `branchRemoteName(branchName: string): string | null` — Retrieve remote name configured for branch.

---

### 6. Index, Status & Checkout
- `indexEntries(): IndexEntry[]` — Read entries in the index.
- `indexOrLoadFromHeadEntries(): IndexEntry[]` — Read index or load from HEAD tree if missing.
- `editIndex(entries: IndexEntryInput[]): void` — Mutate index entries and write to disk.
- `status(): Promise<StatusEntry[]>` — Asynchronously compute working tree status.
- `checkout(target: string): Promise<string>` — Asynchronously checkout commit to working tree and index.
- `reset(target: string, hard?: boolean): void` — Reset HEAD, index, and optionally working tree.

---

### 7. Commits, History & Revisions
- `commit(reference: string, message: string, tree: string, parents: string[]): string` — Create commit and update ref.
- `revParseSingle(spec: string): string` — Resolve revision string (e.g. `HEAD~1`, `main`) to OID.
- `revParseHex(spec: string): string` — Alias for `revParseSingle`.
- `revParse(spec: string): RevSpec` — Parse revision specification into kind (`single`, `range`, `merge`) and OIDs.
- `revWalk(tips: string[], options?: WalkOptions): string[]` — Walk reachable commits with sort/reverse/first-parent.
- `log(rev?: string, limit?: number, firstParent?: boolean): LogEntry[]` — Read commit history with decoded metadata.
- `isAncestor(ancestor: string, descendant: string): boolean` — Check if `ancestor` is ancestor of `descendant`.
- `describe(id: string): string | null` — Format commit ID using `git describe`.
- `mergeBase(one: string, two: string): string | null` — Compute merge base of two commits.
- `mergeBaseOctopus(commits: string[]): string | null` — Compute octopus merge base of multiple commits.
- `mergeBaseWithGraph(one: string, two: string): string | null` — Merge base using explicit revision graph.
- `mergeBasesMany(one: string, others: string[]): string[]` — All merge bases between commit and others.
- `mergeBasesManyWithGraph(one: string, others: string[]): string[]` — All merge bases using revision graph.
- `mergeBaseOctopusWithGraph(commits: string[]): string | null` — Octopus merge base using revision graph.
- `commitGraph(): CommitGraphInfo | null` — Info on commit-graph if present.
- `commitGraphIfEnabled(): CommitGraphInfo | null` — Info on commit-graph if enabled by config.

---

### 8. Merge & Diff Operations
- `mergeTrees(ourTree: string, theirTree: string, ancestorTree: string): MergeOutcome` — Merge two trees with ancestor.
- `mergeCommits(ourCommit: string, theirCommit: string): MergeOutcome` — Merge two commits.
- `diffTreeToTree(oldTreeId: string | null, newTreeId: string): Change[]` — Diff two trees or tree against empty.
- `blame(filePath: string, commitId?: string): BlameOutcome` — Compute line-by-line blame for file.
- `blobMergeOptions(): BlobMergeOptionsSummary` — Resolved text merge configuration.
- `virtualMergeBase(mergeBases: string[]): VirtualMergeBaseOutcome` — Recursively merge merge-bases into virtual commit.
- `virtualMergeBaseWithGraph(mergeBases: string[]): VirtualMergeBaseOutcome` — Virtual merge base with graph cache.

---

### 9. Attributes, Excludes & Location
- `attributes(relativePath: string): AttributeEntry[]` — Query git attributes for path.
- `isPathExcluded(relativePath: string): boolean` — Check if path is ignored/excluded.
- `workDir(): string | null` — Path to working directory, or null if bare.
- `gitDir(): string` — Path to `.git` directory.
- `commonDir(): string` — Path to common git dir (differs for linked worktrees).
- `isBare(): boolean` — Whether repository is bare.
- `kind(): string` — Repository kind (`Common` / `Bare`).
- `objectHash(): string` — Object hash algorithm (`Sha1` / `Sha256`).
- `gitDirTrust(): string` — Trust level (`full` / `reduced`).
- `currentDir(): string` — Repository current directory.
- `modulesPath(): string | null` — Path to `.git/modules`, or null.
- `installDir(): string` — Git binary installation directory.
- `prefix(): string | null` — Relative path prefix from worktree root to cwd.
- `normalizePath(path: string): string` — Normalize path relative to repository root.
- `isPristine(): boolean | null` — Check if repository is pristine (unmodified template state).
- `workdirPath(relativePath: string): string | null` — Resolve relative path against workdir.
- `setWorkdir(workdir?: string): string | null` — Replace working tree path.
- `state(): string | null` — In-progress operation state (`Merge`, `Rebase`, etc.).
- `pathspecDefaults(): PathspecDefaults` — Pathspec configuration defaults.
- `isShallow(): boolean` — Check if repository is a shallow clone.
- `shallowFile(): string` — Path to `shallow` file.
- `shallowCommits(): string[] | null` — Boundary commit IDs for shallow clone.
- `worktrees(): WorktreeProxy[]` — List secondary worktrees.
- `isWorktree(): boolean` — Whether current repository instance is a linked worktree.
- `mainRepository(): Repository` — Main repository handle if in a linked worktree.
- `submodules(): SubmoduleInfo[]` — List submodules.
- `submoduleByName(name: string): SubmoduleInfo | null` — Find submodule by name.
- `defaultAuthor(): Signature | null` — Default configured author signature.
- `defaultCommitter(): Signature | null` — Default configured committer signature.
- `findNote(targetId: string): NoteInfo | null` — Retrieve attached Git note.

---

## Missing gix `Repository` API Audit & Roadmap

The following lower-level or specialized gix 0.89 `Repository` areas are identified for future extension:

| Domain | gix Method / Feature | Purpose / Description |
|---|---|---|
| **Mailmap** | `open_mailmap()` / `mailmap()` | Author/committer identity canonicalization via `.mailmap` files. |
| **Filter Pipeline** | `filter_pipeline()` | Smudge/clean/crlf/ident transformation pipeline for blob content. |
| **Pathspec Search** | `pathspec()` | Complex pattern matching on index, tree entries, or working directory. |
| **Dirwalk** | `dirwalk()` | Directory traversal for discovering untracked, ignored, or unignored files. |
| **Rebase & Sequencer** | `rebase()` / `cherry_pick()` / `apply()` | Patch application and commit sequence manipulation. |
| **Worktree Mutation** | `create_worktree()` / `remove_worktree()` | Adding and pruning linked secondary worktrees. |
| **ODB Verification** | `verify()` / `valid_objects()` | Integrity checking and packfile verification. |
| **Config Resolution** | `config_path(source)` | Explicit path resolution for user/system/local config files. |
| **Reflog Reading** | `Reference::log()` | Reading full reflog history per reference. |
