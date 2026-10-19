# Repository API Documentation (`gix` & Node.js Bindings)

This document provides a comprehensive overview of the `gix` (gitoxide) Rust crate API, its exported structures and functions, how `gix::Repository` is mapped to Node.js / TypeScript N-API bindings, and practical JavaScript/TypeScript usage examples.

---

## 1. Overview of `gix` (gitoxide) API

`gix` is the high-level idiomatic Rust interface for the gitoxide repository. It provides thread-safe repository access, object decoding, reference inspection and manipulation, revision walking, diffing, remote fetching, and cryptographic commit signing and signature verification.

### Core Architecture & Types

- **`gix::ThreadSafeRepository`**: A `Send + Sync` thread-safe handle wrapping repository state (`Arc<ThreadSafeRepository>`). Can be freely passed across worker threads and async task pools.
- **`gix::Repository`**: A cheap, thread-local view (`ThreadSafeRepository::to_thread_local()`) providing the primary synchronous API for reading objects, references, configs, diffs, and commits.
- **Static Constructors / Openers**:
  - `gix::open(path)` / `ThreadSafeRepository::open(path)`: Opens an existing repository at `path` (or `path/.git`).
  - `gix::init(path)` / `gix::init_bare(path)`: Initializes a non-bare working tree or bare git repository.
  - `gix::discover(path)`: Walks up directory hierarchies from `path` to locate an enclosing git repository.
  - `gix::prepare_clone(url, path)` / `gix::prepare_clone_bare(url, path)`: Configures and executes clone operations over native transports.

---

## 2. Exported Structs & API Surface

### Primary Classes / Structs

| Struct / Class | Description |
| :--- | :--- |
| **`Repository`** | Main entry point for repository operations: object lookup, references, remotes, diffs, revision walking, and history logs. |
| **`Head`** | Represents `HEAD` state (attached symbolic reference, detached `ObjectId`, or unborn branch). |
| **`Commit`** | Decoded commit object. Exposes author/committer signatures, parent commit IDs, tree ID, signature verification (`verify_signature`), and commit signing (`sign`). |
| **`Tree` & `TreeEntry`** | Decoded git tree object and directory entry list (`filename`, `oid`, `mode`). |
| **`Blob`** | Raw git blob object wrapper providing lazy binary content retrieval (`content()`). |
| **`Tag`** | Decoded annotated tag object (`name`, `target_id`, `target_kind`, `tagger`, `message`). |
| **`Reference`** | Direct or symbolic reference (`name`, `target`, `peeled`, `peel()`, `delete()`). |
| **`Remote`** | Configured remote definition (`name`, `url`) with async `fetch()` capability. |
| **`GitObject`** | Generic git object wrapper with downcast helpers (`as_commit()`, `as_tree()`, `as_blob()`, `as_tag()`). |
| **`Signature`** | Author / Committer / Tagger signature details (`name`, `email`, `time`, `timezone_offset`). |
| **`SignatureVerification`** | Commit signature status (`signed`, `format`, `status`, `trust`, `signer`, `key`, `valid`, `output`). |
| **`LogEntry`** | Reverse-chronological commit summary returned by `log()`. |
| **`Change`** | Single tree diff item produced by `diff_tree_to_tree()` (`path`, `old_id`, `new_id`, `kind`). |

---

## 3. JavaScript / TypeScript Examples

### 3.1 Static Factory Constructors (`open`, `init`, `discover`, `clone`)

```typescript
import { Repository } from "./main.ts";

// 1. Open an existing repository
const repo = Repository.open("/path/to/repo");

// 2. Initialize a new working-tree or bare repository
const newRepo = Repository.init("/path/to/new-repo", false); // non-bare
const bareRepo = Repository.init("/path/to/bare-repo.git", true); // bare

// 3. Discover repository walking up from current or subdirectory
const discovered = Repository.discover("./subdir");

// 4. Clone a repository asynchronously
const clonedRepo = await Repository.clone(
  "https://github.com/example/repo.git",
  "/path/to/target",
  false
);
```

---

### 3.2 Resolving Revisions and Reading Specific File Entries (e.g. `ReadMe.md` from `main` or SHA)

`Repository.prototype.revParseSingle(spec)` parses standard git revision expressions, including branch names (`"main"`), commit SHAs (`"45c58a7"`), parent specs (`"HEAD~1"`), and path specifications (`"main:ReadMe.md"`, `"<SHA>:ReadMe.md"`, `"HEAD:repository.manifest.yml"`).

```typescript
import { Repository } from "./main.ts";

const repo = Repository.open(process.cwd());

// 1. Resolve commit SHA from a branch or ref
const mainCommitId = repo.revParseSingle("main");
console.log(`Latest commit on main: ${mainCommitId}`);

// 2. Resolve a specific file entry (ReadMe.md) from the "main" branch or SHA
const readmeBlobId = repo.revParseSingle("main:ReadMe.md");
const readmeBlob = repo.findBlob(readmeBlobId);
const readmeText = readmeBlob.content().toString("utf-8");
console.log("ReadMe.md from main:\n", readmeText);

// 3. Resolve a file entry at a specific commit SHA
const specificSha = "45c58a7a137f5d8b8c082fa75aa5b8644fd18dd6";
const manifestBlobId = repo.revParseSingle(`${specificSha}:repository.manifest.yml`);
const manifestContent = repo.findBlob(manifestBlobId).content().toString("utf-8");
console.log("Manifest at SHA:\n", manifestContent);
```

---

### 3.3 Reading Git Logs (`repo.log()`)

`Repository.prototype.log(rev?, limit?, firstParent?)` retrieves commit summaries in reverse-chronological order without needing manual per-commit round-trips.

```typescript
import { Repository, type LogEntry } from "./main.ts";

const repo = Repository.open(process.cwd());

// Read top 10 commits starting at HEAD
const logs: LogEntry[] = repo.log("HEAD", 10, false);

for (const entry of logs) {
  console.log(`Commit: ${entry.id}`);
  console.log(`Subject: ${entry.subject}`);
  console.log(`Author: ${entry.authorName} <${entry.authorEmail}>`);
  console.log(`Date: ${new Date(entry.authorTime * 1000).toISOString()}`);
  console.log(`Parents: ${entry.parentIds.join(", ") || "none"}`);
  console.log("---");
}

// First-parent walk (useful for linear branch history)
const linearLogs = repo.log("main", 20, true);
```

---

### 3.3 Creating and Inspecting Annotated Tags, Objects, and Refs

```typescript
import { Repository } from "./main.ts";

const repo = Repository.open(process.cwd());
const commit = repo.headCommit();

// 1. Create an annotated tag pointing to a commit
const tagRef = repo.createTag(
  "v1.0.0",
  commit.id,
  "Release version 1.0.0",
  true // force overwrite if tag exists
);

console.log(`Tag reference created: ${tagRef.name}`); // refs/tags/v1.0.0
console.log(`Tag object ID: ${tagRef.target}`);

// 2. Read and inspect the Tag object
const tagObj = repo.findTag(tagRef.target!);
console.log(`Tag Name: ${tagObj.name}`);
console.log(`Target Commit: ${tagObj.targetId}`);
console.log(`Tag Message: ${tagObj.message}`);

// 3. Downcast via findObject -> asTag()
const genericObj = repo.findObject(tagRef.target!);
if (genericObj.kind === "tag") {
  const tag = genericObj.asTag();
  console.log(`Tagger Name: ${tag?.tagger?.name}`);
}

// 4. Peel tag reference back to the target commit ID
const targetCommitId = tagRef.peel();
console.log(`Peeled target commit ID: ${targetCommitId}`);

// 5. Create a lightweight tag
const lwRef = repo.createLightweightTag("v1.0.0-lw", commit.id, true);

// 6. Delete tag reference
tagRef.delete();
lwRef.delete();
```

---

### 3.4 Verifying and Signing Commits

```typescript
import { Repository } from "./main.ts";

const repo = Repository.open(process.cwd());
const commit = repo.headCommit();

// Verify commit signature
const verification = commit.verifySignature();
if (verification.signed) {
  console.log(`Signed by: ${verification.signer ?? "unknown"}`);
  console.log(`Valid signature: ${verification.valid}`);
  console.log(`Format: ${verification.format}`);
} else {
  console.log("Commit is unsigned.");
}

// Create a signed copy of a commit (requires user.signingKey in git config)
try {
  const signedCommit = commit.sign();
  console.log(`Created signed commit: ${signedCommit.id}`);
} catch (err) {
  console.error("Signing failed:", err);
}
```

---

### 3.5 Configuring Remotes from Manifest (`repository.manifest.yml`)

```typescript
import { Repository } from "./main.ts";

const repo = Repository.open(process.cwd());

// Read repository.manifest.yml and set up remotes (origin & named remotes)
repo.setupRemotesFromManifest("repository.manifest.yml");

for (const remote of repo.remotes()) {
  console.log(`Remote: ${remote.name} -> ${remote.url}`);
}
```
