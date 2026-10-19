# Repository API & `gix` Binding Architecture

This document describes the structure and API mappings of the Node.js `repository` binding backed by [gitoxide (`gix`)](https://docs.rs/gix/latest/gix/).

---

## 1. `gix` Crate Architecture Overview

In `gix` (gitoxide), repository handles and objects are designed around safety and thread efficiency:

* **`gix::ThreadSafeRepository`**: A thread-safe, cloneable handle to a git repository (`Arc`-backed). It can be shared across worker threads for async operations (e.g. network clone/fetch/push).
* **`gix::Repository`**: A cheap, thread-local view created from `ThreadSafeRepository::to_thread_local()`. Sync operations (walking revs, finding commits, reading config/refs) execute on this view.
* **Creation / Initialization**: Opening or initializing a repo is fallible in `gix` via top-level functions or static factories:
  * `gix::open(path)` / `ThreadSafeRepository::open(path)`
  * `gix::init(path)` / `gix::init_bare(path)`
  * `gix::discover(path)`
  * `gix::prepare_clone(url, path)` / `gix::prepare_clone_bare(url, path)`

---

## 2. Rationale for Static Constructors

NAPI-RS constructors (`new Repository(...)`) cannot return errors cleanly or perform asynchronous thread-pool tasks. Thus, all creation is exposed via **static factory methods** on `Repository`:

```ts
import { Repository } from "repository";

// Static Constructors
const repo1 = Repository.open("./path/to/repo");
const repo2 = Repository.init("./path/to/new-repo", false); // bare = false
const repo3 = Repository.discover("./path/to/subdir");
const repo4 = await Repository.clone("https://github.com/example/repo.git", "./cloned");
```

---

## 3. Core Classes & API Mapping

| Class / Type | Responsibilities | Key Methods / Properties |
| ------------ | ---------------- | ------------------------ |
| **`Repository`** | Primary handle to a git repository | `Repository.open`, `Repository.init`, `Repository.discover`, `Repository.clone`<br>`repo.head()`, `repo.headCommit()`, `repo.headId()`, `repo.headName()`<br>`repo.findCommit(id)`, `repo.findTree(id)`, `repo.findBlob(id)`, `repo.findTag(id)`, `repo.findObject(id)`<br>`repo.writeBlob(buffer)`, `repo.commit(...)`<br>`repo.references()`, `repo.findReference(name)`, `repo.createReference(...)`<br>`repo.remotes()`, `repo.findRemote(name)`<br>`repo.log(rev?, limit?, firstParent?)`, `repo.revWalk(tips)`, `repo.reflog(name?)`<br>`repo.revParseSingle(spec)` / `repo.revParse(spec)`<br>`repo.diffTreeToTree(old?, new)` |
| **`Commit`** | Decoded commit object with back-link to repository | `commit.id`, `commit.message`, `commit.treeId`, `commit.parentIds`<br>`commit.author`: `Signature`<br>`commit.committer`: `Signature`<br>`commit.parents()`: `Commit[]`<br>`commit.tree()`: `Tree` |
| **`Tree`** | Decoded tree object | `tree.id`, `tree.entries`: `TreeEntry[]`, `tree.entry(name)` |
| **`Blob`** | Raw file content handle | `blob.id`, `blob.content()`: `Buffer` |
| **`Tag`** | Annotated tag handle | `tag.id`, `tag.name`, `tag.targetId`, `tag.targetKind`, `tag.message`, `tag.tagger` |
| **`Head`** | HEAD reference state | `head.name`, `head.referent`, `head.id`, `head.commit()`, `head.tree()` |
| **`Reference`** | Git reference | `ref.name`, `ref.target`, `ref.peeled`, `ref.peel()`, `ref.delete()`, `ref.reflog()` |
| **`Remote`** | Configured remote | `remote.name`, `remote.url`, `remote.fetch()`: `Promise<RemoteFetchOutcome>` |

---

## 4. Reading Specific File Revisions (e.g. `ReadMe.md` from `main` or SHA)

To read a file entry from a specific branch or commit SHA:

```js
import { Repository } from "repository";

const repo = Repository.open(".");

// 1. Resolve a branch (e.g. "main" or "refs/heads/main") or a commit SHA
const commitOid = repo.revParseSingle("main"); // or repo.revParseSingle("<commit-sha>")

// 2. Fetch the commit and its root tree
const commit = repo.findCommit(commitOid);
const tree = commit.tree();

// 3. Find the file entry inside the tree
const entry = tree.entry("ReadMe.md");
if (entry) {
  // 4. Fetch blob content
  const blob = repo.findBlob(entry.id);
  const text = blob.content().toString("utf-8");
  console.log("ReadMe.md content:\n", text);
}
```

---

## 5. Reading Git Logs & Reflogs in JavaScript

### Option A: `repo.log(rev?, limit?, firstParent?)`
Resolves commit summaries in one efficient native call (reverse-chronological order):

```js
import { Repository } from "repository";

const repo = Repository.open(".");

// Fetch up to 20 newest commits starting at HEAD
const entries = repo.log(undefined, 20, false);

for (const entry of entries) {
  const date = new Date(entry.authorTime * 1000).toISOString();
  console.log(`${entry.id.slice(0, 8)} | ${date} | ${entry.authorName} | ${entry.subject}`);
}
```

### Option B: `repo.reflog(name?)`
Read reference logs (reflog) for `HEAD` or any reference:

```js
const mainReflog = repo.reflog("refs/heads/main");
for (const entry of mainReflog) {
  console.log(`${entry.previousOid.slice(0, 8)} -> ${entry.newOid.slice(0, 8)}: ${entry.message}`);
}
```

---

## 6. Commit Verification and Signatures

Every commit object decodes author and committer signatures into `Signature` objects:

```js
interface Signature {
  name: string;
  email: string;
  time: number; // Unix timestamp in seconds
  timezoneOffset: number; // Offset in minutes
}
```
