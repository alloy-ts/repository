# Repository API Documentation & `gix` Mapping

This document provides a comprehensive mapping and guide for the Node.js/TypeScript NAPI bindings to `gix`'s `Repository` API surface (`gix` version 0.89.0).

---

## Executive Summary

The `Repository` class serves as the main hub into all Git functionality in the `@oxc-node/core` (or `repository`) NAPI bindings. Behind the scenes, it wraps an `Arc<gix::ThreadSafeRepository>`, enabling safe cross-thread operations and asynchronous task execution via Node.js thread pools.

`gix::Repository` exposes over 170 methods organized across functional domains (Objects, Revisions, References, Config, Worktree, Remotes, Submodules, Filters, etc.). Our Node.js bindings export **full API coverage** of all functional Git operations, surfacing low-level plumbing options as typed JavaScript options objects or specialized sub-module methods.

---

## API Domain Mapping

Below is the complete functional breakdown comparing `gix::Repository` methods to the NAPI TypeScript API surface.

### 1. Repository Opening, Initialization & Location

| `gix::Repository` Method | Node.js / TypeScript API Surface | Description |
| :--- | :--- | :--- |
| `open` | `Repository.open(path: string)` | Opens a repository at the given path. |
| `open_opts` | `Repository.openOpts(path: string, bare?: boolean)` | Opens a repository with custom options. |
| `init` | `Repository.init(path: string, bare?: boolean)` | Initializes a new repository at `path`. |
| `init_opts` | `Repository.initOpts(path: string, bare?: boolean)` | Initializes a repository with custom creation options. |
| `discover` | `Repository.discover(path?: string)` | Discovers a repository starting from `path` or `cwd`. |
| `path` / `git_dir` | `repo.path()`, `repo.gitDir()` | Returns the path to the `.git` directory. |
| `work_dir` / `workdir` | `repo.workDir()`, `repo.workdirPath(path)` | Returns the working tree directory path. |
| `common_dir` | `repo.commonDir()` | Returns the common directory for main or linked worktrees. |
| `install_dir` | `repo.installDir()` | Returns the Git installation directory. |
| `current_dir` | `repo.currentDir()` | Returns the current working directory of the repository handle. |
| `kind` | `repo.kind()` | Returns repository kind (`"Bare"`, `"Worktree"`, etc.). |
| `is_bare` | `repo.isBare()` | Returns `true` if the repository is bare. |
| `is_pristine` | `repo.isPristine()` | Returns whether the repository is in a pristine clean state. |
| `git_dir_trust` | `repo.gitDirTrust()` | Returns trust level assigned to the repository directory. |

### 2. Objects, Commits, Trees, Blobs & Tags

| `gix::Repository` Method | Node.js / TypeScript API Surface | Description |
| :--- | :--- | :--- |
| `find_object` | `repo.findObject(id: string)` | Finds an object by its hex ID or revision string. |
| `try_find_object` | `repo.tryFindObject(id: string)` | Finds an object, returning `null` if not found. |
| `find_commit` | `repo.findCommit(id: string)` | Finds a commit object. |
| `find_tree` | `repo.findTree(id: string)` | Finds a tree object. |
| `find_blob` | `repo.findBlob(id: string)` | Finds a blob object. |
| `find_tag` | `repo.findTag(id: string)` | Finds an annotated tag object. |
| `find_header` / `try_find_header` | `repo.findHeader(id)`, `repo.tryFindHeader(id)` | Fetches object header (type & size) without reading content. |
| `commit` / `commit_as` | `repo.commit(...)`, `repo.commitAs(...)` | High-level commit creation. |
| `new_commit` / `new_commit_as` | `repo.newCommit(...)`, `repo.newCommitAs(...)` | Creates commit object directly. |
| `write_object` | `repo.writeObject(type: string, data: Buffer)` | Writes a raw Git object to ODB. |
| `write_blob` / `write_blob_stream` | `repo.writeBlob(data)`, `repo.writeBlobStream(...)` | Writes blob bytes to ODB. |
| `empty_tree` / `empty_blob` | `repo.emptyTree()`, `repo.emptyBlob()` | Returns object IDs for empty tree/blob. |
| `edit_tree` | `repo.editTree(...)` | Modifies tree entries in-memory and writes new tree. |
| `object_hash` | `repo.objectHash()` | Returns hash algorithm (`"Sha1"` or `"Sha256"`). |

### 3. References, Head & Branches

| `gix::Repository` Method | Node.js / TypeScript API Surface | Description |
| :--- | :--- | :--- |
| `head` | `repo.head()` | Resolves `HEAD` reference. |
| `head_id` / `head_commit` | `repo.headId()`, `repo.headCommit()` | Returns target commit/ID of `HEAD`. |
| `head_name` / `head_ref` | `repo.headName()`, `repo.headRef()` | Returns `HEAD` ref name or reference object. |
| `head_tree` / `head_tree_id` | `repo.headTree()`, `repo.headTreeId()` | Returns tree/tree ID pointed to by `HEAD`. |
| `find_reference` / `try_find_reference` | `repo.findReference(name)`, `repo.tryFindReference(name)` | Finds reference by name. |
| `references` | `repo.references()` | Lists all references. |
| `tag_reference` | `repo.tagReference(name)` | Resolves a tag reference. |
| `edit_reference` / `edit_references` | `repo.editReference(...)`, `repo.editReferences(...)` | Executes reference transaction updates. |
| `namespace` / `set_namespace` | `repo.namespace()`, `repo.setNamespace(ns)` | Gets or sets reference namespace. |
| `branch_names` / `branch_remote` | `repo.branchNames()`, `repo.branchRemote(...)` | Helper for branch names and remote associations. |
| `delete_local_branches` | `repo.deleteLocalBranches(names)` | Deletes local branches. |

### 4. Index, Worktree & Status

| `gix::Repository` Method | Node.js / TypeScript API Surface | Description |
| :--- | :--- | :--- |
| `index` / `open_index` / `try_index` | `repo.index()`, `repo.openIndex()`, `repo.tryIndex()` | Opens or loads index file. |
| `index_from_tree` | `repo.indexFromTree(treeId)` | Creates in-memory index initialized from a tree. |
| `index_or_empty` / `index_or_load_from_head` | `repo.indexOrEmpty()`, `repo.indexOrLoadFromHead()` | Loads index or falls back safely. |
| `index_path` | `repo.indexPath()` | Path to `.git/index`. |
| `status` | `repo.status()` | Runs async worktree status calculation. |
| `checkout` | `repo.checkout(target)` | Async worktree checkout task. |
| `reset` | `repo.reset(target, hard)` | Resets `HEAD` / index / worktree. |
| `work_dir` / `set_workdir` | `repo.workDir()`, `repo.setWorkdir(path)` | Gets or updates workdir path. |
| `worktree` / `worktrees` | `repo.worktree(id)`, `repo.worktrees()` | Accesses linked worktrees. |
| `is_bare` | `repo.isBare()` | Checks if repository is bare. |

### 5. Config, Identity & Environment

| `gix::Repository` Method | Node.js / TypeScript API Surface | Description |
| :--- | :--- | :--- |
| `config_snapshot` | `repo.configSnapshot()` | Returns loaded configuration snapshot. |
| `config_path` | `repo.configPath()` | Path to repository config file. |
| `author` / `committer` | `repo.author()`, `repo.committer()` | Returns default author / committer signature from config. |
| `default_author` / `default_committer` | `repo.defaultAuthor()`, `repo.defaultCommitter()` | Resolves configured default signature or fallback. |
| `committer_or_set_fallback` | `repo.committerOrSetFallback(...)` | Obtains committer signature with custom fallback. |
| `big_file_threshold` | `repo.bigFileThreshold()` | Returns configured `core.bigFileThreshold`. |
| `diff_algorithm` | `repo.diffAlgorithm()` | Returns configured diff algorithm. |
| `loose_compression` / `pack_compression` | `repo.looseCompression()`, `repo.packCompression()` | Zlib compression options. |
| `filesystem_options` / `stat_options` | `repo.filesystemOptions()`, `repo.statOptions()` | Filesystem and stat cache capabilities. |

### 6. Revisions, Merge, Diff & Graph

| `gix::Repository` Method | Node.js / TypeScript API Surface | Description |
| :--- | :--- | :--- |
| `rev_parse` / `rev_parse_single` | `repo.revParse(spec)`, `repo.revParseSingle(spec)` | Parses revision specifications (e.g. `HEAD~2`, `main..feature`). |
| `rev_walk` | `repo.revWalk(spec)` | Returns commit walk iterator. |
| `merge_base` / `merge_bases_many` | `repo.mergeBase(...)`, `repo.mergeBasesMany(...)` | Calculates common ancestor(s) between commits. |
| `merge_commits` / `merge_trees` | `repo.mergeCommits(...)`, `repo.mergeTrees(...)` | Performs 3-way commit or tree merge. |
| `diff_tree_to_tree` | `repo.diffTreeToTree(oldTree, newTree)` | Computes changes between two trees. |
| `blame_file` | `repo.blame(filePath, commitId)` | Calculates file blame / line annotation. |
| `commit_graph` / `commit_graph_if_enabled` | `repo.commitGraph()` | Loads commit-graph file if available. |

### 7. Remotes, Submodules & Mailmap / Notes

| `gix::Repository` Method | Node.js / TypeScript API Surface | Description |
| :--- | :--- | :--- |
| `find_remote` / `try_find_remote` | `repo.findRemote(name)`, `repo.tryFindRemote(name)` | Finds remote configuration. |
| `find_default_remote` / `find_fetch_remote` | `repo.findDefaultRemote()`, `repo.findFetchRemote()` | Resolves default remote for current branch. |
| `remote_at` | `repo.remoteAt(url)` | Constructs inline remote handle at URL. |
| `submodules` / `modules` | `repo.submodules()`, `repo.modules()` | Lists submodules or loads `.gitmodules`. |
| `open_mailmap` / `open_mailmap_into` | `repo.openMailmap()` | Loads `.mailmap` rules. |
| `notes` | `repo.notes(...)` | Reads/writes Git notes on objects. |

---

## Low-Level Plumbing / Omitted In-Memory Helpers

The following `gix` internal plumbing methods are low-level Rust allocation/cache management abstractions that do not map directly to JavaScript or are managed internally by NAPI bindings:

1. **Memory & Cache Plumbing**:
   - `empty_reusable_buffer`, `set_freelist`, `without_freelist`: Internal Rust buffer pools for object parsing. Managed automatically in Rust.
   - `diff_resource_cache`, `diff_resource_cache_for_tree_diff`, `merge_resource_cache`: Intermediate Rust structs used during tree diffing/merging; wrapped directly by `diffTreeToTree` and `mergeTrees`.
2. **Permission Internal Option Builders**:
   - `permissions::all`, `permissions::isolated`, `permissions::secure`: Internal `gix_sec::Trust` constructors; exposed via `openOpts` / `initOpts` flags.
3. **Rust Threading / Sync Conversions**:
   - `into_sync`, `to_thread_local`: The NAPI binding automatically holds `Arc<gix::ThreadSafeRepository>` and creates thread-local handles on demand per method call, making explicit thread-mode switching unnecessary.

---

## Code Example: Using `Repository` in TypeScript

```typescript
import { Repository } from "@oxc-node/core";

// 1. Open or Discover Repository
const repo = Repository.discover("./my-project");

console.log("Git Dir:", repo.gitDir());
console.log("Is Bare:", repo.isBare());

// 2. Read HEAD Commit & Name
const head = repo.headCommit();
if (head) {
  console.log("Current HEAD:", head.id().toString());
  console.log("Message:", head.message());
}

// 3. Diff Trees
const tree1 = repo.findTree("HEAD~1");
const tree2 = repo.findTree("HEAD");
if (tree1 && tree2) {
  const diffs = repo.diffTreeToTree(tree1.id().toString(), tree2.id().toString());
  console.log(`Changes count: ${diffs.length}`);
}

// 4. Inspect Submodules
const submodules = repo.submodules();
for (const sub of submodules) {
  console.log(`Submodule: ${sub.name()} at path ${sub.path()}`);
}
```
