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
| **`Repository`** | Primary handle to a git repository | `Repository.open`, `Repository.init`, `Repository.discover`, `Repository.clone`<br>`repo.head()`, `repo.headCommit()`, `repo.headId()`, `repo.headName()`<br>`repo.findCommit(id)`, `repo.findTree(id)`, `repo.findBlob(id)`, `repo.findTag(id)`, `repo.findObject(id)`<br>`repo.writeBlob(buffer)`, `repo.commit(...)`<br>`repo.references()`, `repo.findReference(name)`, `repo.createReference(...)`<br>`repo.remotes()`, `repo.findRemote(name)`<br>`repo.log(rev?, limit?, firstParent?)`, `repo.revWalk(tips)`<br>`repo.diffTreeToTree(old?, new)` |
| **`Commit`** | Decoded commit object with back-link to repository | `commit.id`, `commit.message`, `commit.treeId`, `commit.parentIds`<br>`commit.author`: `Signature`<br>`commit.committer`: `Signature`<br>`commit.parents()`: `Commit[]`<br>`commit.tree()`: `Tree` |
| **`Tree`** | Decoded tree object | `tree.id`, `tree.entries`: `TreeEntry[]`, `tree.entry(name)` |
| **`Blob`** | Raw file content handle | `blob.id`, `blob.content()`: `Buffer` |
| **`Tag`** | Annotated tag handle | `tag.id`, `tag.name`, `tag.targetId`, `tag.targetKind`, `tag.message`, `tag.tagger` |
| **`Head`** | HEAD reference state | `head.name`, `head.referent`, `head.id`, `head.commit()`, `head.tree()` |
| **`Reference`** | Git reference | `ref.name`, `ref.target`, `ref.peeled`, `ref.peel()`, `ref.delete()` |
| **`Remote`** | Configured remote | `remote.name`, `remote.url`, `remote.fetch()`: `Promise<RemoteFetchOutcome>` |

---

## 4. Reading Git Logs in JavaScript

`Repository` provides two complementary ways to read git logs:

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

### Option B: `repo.revWalk(tips)` + `repo.findCommit(id)`
Walk object IDs manually and inspect individual rich `Commit` instances:

```js
import { Repository } from "repository";

const repo = Repository.open(".");
const headId = repo.headId();

// Walk revision history starting from HEAD
const oids = repo.revWalk([headId]);

for (const oid of oids) {
  const commit = repo.findCommit(oid);
  console.log(`${commit.id.slice(0, 8)} ${commit.message.split("\n")[0]}`);

  // Access parent commits
  const parents = commit.parents();
  console.log(`  Parents: ${parents.map(p => p.id.slice(0, 8)).join(", ")}`);
}
```

---

## 5. Commit Verification and Signatures

Every commit object decodes author and committer signatures into `Signature` objects:

```js
interface Signature {
  name: string;
  email: string;
  time: number; // Unix timestamp in seconds
  timezoneOffset: number; // Offset in minutes
}
```

Commit headers and metadata can be inspected to verify commit structure, author timestamps, and parent graph integrity.
