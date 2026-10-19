# repository

Node.js bindings for [gitoxide (`gix`)](https://docs.rs/gix/latest/gix/) — a `Repository` class for reading git history, objects, references, config, status, worktrees, submodules, and performing async clone/fetch/push, built with [napi-rs](https://napi.rs).

## Install & build

```bash
npm install
npm run build        # compiles the cdylib and emits dist/*.node + .d.ts + main.mjs
```

Running `npm run build` requires a Rust toolchain and network access (it fetches and compiles `gix` and its sub-crates).

## Usage

A `Repository` is created via **static factory methods** (opening a repo is fallible, so there is no `new Repository(...)`):

```ts
import { Repository } from "repository";

// Open / discover / init
const repo = Repository.open(".");
// const repo = Repository.discover();        // walk up to find .git
// const repo = Repository.init("./new-repo");

// HEAD & commit decoding
const headCommit = repo.headCommit();
const decoded = headCommit.decode();
console.log("Head commit author:", decoded.author.name);

// Read the commit log (one call, newest first)
const entries = repo.log(undefined, 20, false);
for (const e of entries) {
  const when = new Date(e.authorTime * 1000).toISOString().slice(0, 10);
  console.log(`${e.id.slice(0, 8)} ${when} ${e.authorName} ${e.subject}`);
}

// Working tree status (async libuv thread pool)
const statusList = await repo.status();
for (const s of statusList) {
  console.log(`${s.path}: ${s.status}`);
}

// Network operations are async (libuv thread pool)
const fresh = await Repository.clone("https://github.com/example/repo.git", "./clone");
const remote = repo.findRemote("origin");
const outcome = await remote.fetch();
```

## API shape

| Category | Example (TypeScript) |
|----------|----------------------|
| **Static constructors** | `Repository.open(path)`, `Repository.init(path, bare?)`, `Repository.discover(path?)`, `Repository.clone(url, path, bare?)` |
| **HEAD & objects** | `repo.head()`, `repo.headCommit()`, `repo.headTreeId()`, `repo.headRef()`, `repo.headTree()`, `repo.setHead(target)`, `repo.findCommit(id)`, `repo.findTree(id)`, `repo.findBlob(id)`, `repo.findTag(id)`, `repo.findObject(id)`, `repo.tryFindObject(id)`, `repo.objectExists(id)` |
| **Commit decoding & signing** | `commit.decode()`, `commit.verifySignature()`, `commit.sign()`, `commit.parents()`, `commit.tree()` |
| **Object writing** | `repo.writeBlob(data)`, `repo.writeTree(entries)`, `repo.commit(ref, msg, tree, parents)` |
| **Index & Status** | `repo.indexEntries()`, `repo.indexOrLoadFromHeadEntries()`, `repo.status()` (async) |
| **References & Tags** | `repo.references()`, `repo.findReference(name)`, `repo.createReference(name, target)`, `repo.createTag(...)`, `repo.createLightweightTag(...)` |
| **Remotes** | `repo.remotes()`, `repo.findRemote(name)`, `repo.remoteAt(url)`, `repo.createRemote(name, url)`, `remote.fetch()` (async) |
| **Revisions & Log** | `repo.revParseSingle(spec)`, `repo.revParseHex(spec)`, `repo.revWalk(tips)`, `repo.mergeBase(a, b)`, `repo.mergeBaseOctopus(ids)`, `repo.isAncestor(a, b)`, `repo.describe(id?)`, `repo.isShallow()`, `repo.log(rev?, limit?, firstParent?)` |
| **Merges** | `repo.mergeTrees(ours, theirs, ancestor)`, `repo.mergeCommits(ours, theirs)` |
| **Config & Branch** | `repo.configString(key)`, `repo.configSet(key, value)`, `repo.configEntries()`, `repo.branchNames()`, `repo.branchRemoteName(branch)` |
| **Attributes & Excludes** | `repo.attributes(path)`, `repo.isPathExcluded(path)` |
| **Worktree & Submodules** | `repo.worktrees()`, `repo.isWorktree()`, `repo.mainRepository()`, `repo.submodules()`, `repo.defaultAuthor()`, `repo.defaultCommitter()` |
| **Manifest Utils** | `setupRemotesFromManifest(repo, revisionOrPath?)` |

Rich object classes (`Commit`, `Tree`, `Blob`, `Tag`, `Reference`, `Head`, `Remote`) keep a link back to the owning `Repository`, so e.g. `commit.parents()` returns resolved `Commit` instances.

> **Why static factories, not `new Repository(...)`?** In `gix`, opening or discovering a repository is a fallible operation (`gix::open`, `gix::discover`). In `napi-rs`, class constructors (`constructor()`) cannot return errors cleanly or execute asynchronous fallible operations. Exposing static factory methods (`Repository.open`, `Repository.init`, `Repository.clone`) allows returning idiomatic JavaScript `Error` objects on failure. See [`docs/api/repository.md`](./docs/api/repository.md) for full design rationale and gix↔binding mapping.

## Testing

```bash
npm run test
```

## Cargo features

The crate enables the gix features needed for the comprehensive surface (`revision`, `merge`, `blob-diff`, `attributes`, `status`, `worktree-mutation`, `blocking-network-client`, `command`). See `Cargo.toml`.
