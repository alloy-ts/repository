# repository

Node.js bindings for [gitoxide (`gix`)](https://docs.rs/gix/latest/gix/) — a `Repository` class for reading git history, objects, references, config, and performing async clone/fetch/push, built with [napi-rs](https://napi.rs).

## Install & build

```bash
npm install
npm run build:napi   # compiles the cdylib and emits dist/*.node + .d.ts
```

Running `build:napi` requires a Rust toolchain and network access (it fetches and compiles `gix` and its sub-crates, which is heavy the first time).

## Usage

A `Repository` is created via **static factory methods** (opening a repo is fallible, so there is no `new Repository(...)`):

```ts
import { Repository, setupRemotes } from "repository";

// Open / discover / init
const repo = Repository.open(".");
// const repo = Repository.discover();        // walk up to find .git
// const repo = Repository.init("./new-repo");

// Read the commit log (one call, newest first)
const entries = repo.log(undefined, 20, false);
for (const e of entries) {
  const when = new Date(e.authorTime * 1000).toISOString().slice(0, 10);
  console.log(`${e.id.slice(0, 8)} ${when} ${e.authorName} ${e.subject}`);
}

// Or walk ids manually and resolve each commit
const ids = repo.revWalk([repo.headId()]);
for (const id of ids) {
  const c = repo.findCommit(id);
  console.log(c.message.split("\n")[0], c.author.name);
}

// A "tagged commit": create the commit, then annotate it with a tag object
const treeId = repo.revParseSingle("HEAD^{tree}");
const commitId = repo.commit("HEAD", "release 1.0\n", treeId, [repo.headId()]);
const tagRef = repo.createTag("v1.0", commitId, "release 1.0\n"); // annotated
const tag = repo.findTag(tagRef.target!); // { name, targetKind: "Commit", tagger, message }
console.log(tagRef.name, "->", tag.targetKind, tag.targetId);

// Configure remotes from repository.manifest.yml (first URL -> `origin`,
// the rest named after their host, e.g. `github`), with fetch refspecs
const remotes = setupRemotes(".", "repository.manifest.yml");

// Network operations are async (libuv thread pool)
const fresh = await Repository.clone("https://github.com/example/repo.git", "./clone");
const remote = repo.findRemote("origin");
const outcome = await remote.fetch();
```

## API shape

| Category | Example (TypeScript) |
|----------|----------------------|
| **Static constructors** | `Repository.open(path)`, `Repository.init(path, bare?)`, `Repository.discover(path?)`, `clone(url, path)` |
| **HEAD / objects** | `repo.head()`, `repo.headCommit()`, `repo.findCommit(id)`, `repo.findTree`, `repo.findBlob`, `repo.findTag`, `repo.objectExists(id)` |
| **Write** | `repo.writeBlob(buffer)`, `repo.commit(...)`, `repo.createReference(...)`, `repo.createTag(...)` |
| **References** | `repo.references()`, `repo.findReference(name)`, `repo.createLightweightTag(...)` |
| **Tags** | `repo.createTag(name, target, message, force?)` (annotated), `repo.findTag(id)` → `tag.targetKind` (`Commit` / `Tree` / `Blob` / `Tag`) |
| **Remotes** | `repo.remotes()`, `repo.findRemote(name)` → `remote.fetch()` / `remote.push()` (async) |
| **Manifest** | `setupRemotes(path?, manifest?)` — configures remotes from `repository.manifest.yml` |
| **Revisions** | `repo.revParseSingle(spec)`, `repo.revWalk(tips)`, `repo.mergeBase(a, b)`, `repo.log(rev?, limit?, firstParent?)` |
| **Diff** | `repo.diffTreeToTree(old?, new)` → `Change[]` |
| **Config / paths** | `repo.configString(key)`, `repo.workDir()`, `repo.gitDir()`, `repo.isBare()`, `repo.kind()` |

Rich object classes (`Commit`, `Tree`, `Blob`, `Tag`, `Reference`, `Head`, `Remote`) keep a link back to the owning `Repository`, so e.g. `commit.parents()` returns resolved `Commit` instances.

> **Why factories, not `new`?** gix has no `Repository::open` instance method — creation is a fallible free function (`gix::open`). napi-rs constructors can't return errors, so we expose `Result`-returning static factories (`Repository.open`) that throw a JS `Error` on failure. See [`docs/api/repository.md`](./docs/api/repository.md) §7 for the full design rationale and the gix↔binding mapping.

## Testing

```bash
npm run test
```

Suites: `src/commit.test.ts` (sign/verify), `src/repository.test.ts` (trees, log, init/clone), `src/tag.test.ts` (tags & annotated objects), `src/util.test.ts` (manifest remotes).

## Cargo features

The crate enables the gix features needed for the comprehensive surface (`revision`, `merge`, `blob-diff`, `attributes`, `status`, `worktree-mutation`, `blocking-network-client`). Manifest parsing uses `serde` + `serde_yaml`. See `Cargo.toml`.
