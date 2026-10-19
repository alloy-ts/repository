# gix (Gitoxide) Crate API Reference: Repository

`gix` (the gitoxide crate) provides a thread-safe and performant pure-Rust implementation of Git operations.

## Key Structs

### `gix::ThreadSafeRepository`

The thread-safe handle to a Git repository. It holds shared state (config, object database, packed refs) wrapped in thread-safe data structures (`Arc`).

- **Use Case**: Multi-threaded servers, libuv thread pools, worker threads.
- **Conversion**: Call `.to_thread_local()` or `Repository::from(thread_safe_repo)` to get a light-weight `gix::Repository` view bound to the current thread.

### `gix::Repository`

A thread-local handle offering rich, ergonomic high-level operations on the repository.

- **Lightweight**: Cheap to construct from `ThreadSafeRepository` for thread-local usage.
- **Scope**: Manages thread-local caches (e.g., object database caches, packed-refs cache).

---

## Static Constructors / Factory Methods

- **`gix::open(path)` / `gix::ThreadSafeRepository::open(path)`**:
  Opens an existing Git repository at `path` (or `path/.git`).
- **`gix::init(path)` / `gix::init_bare(path)`**:
  Initializes a new Git repository (with working tree or bare) at `path`.
- **`gix::discover(path)`**:
  Discovers a Git repository by walking upwards from `path`.
- **`gix::prepare_clone(url, path)` / `gix::prepare_clone_bare(url, path)`**:
  Prepares a clone action, returning a `PrepareFetch` builder to fetch objects and set up references.

---

## Main Repository APIs & Capabilities

### 1. Object Lookup & Access

- `r.find_commit(id)` -> `gix::Commit`: Find and decode a commit.
- `r.find_tree(id)` -> `gix::Tree`: Find and decode a tree object.
- `r.find_blob(id)` -> `gix::Blob`: Find a blob object.
- `r.find_tag(id)` -> `gix::Tag`: Find an annotated tag object.
- `r.find_object(id)` -> `gix::Object`: Find an object of any kind (`Commit`, `Tree`, `Blob`, `Tag`).
- `r.has_object(id)` -> `bool`: Check whether an object exists in ODB.
- `r.write_blob(data)` -> `gix::ObjectId`: Write raw bytes as a blob object.

### 2. Revision Parsing & History Walking (Git Log)

- `r.rev_parse_single(spec)` -> `gix::ObjectId`: Parse revision specifiers like `HEAD`, `main`, `v1.0.0`, or commit OID hex.
- `r.rev_walk(tips)` -> `gix::revision::walk::Platform`: Construct a revision walker starting from tip OIDs.
- `r.merge_base(one, two)` / `r.merge_base_octopus(commits)`: Calculate merge bases between commits.

#### JavaScript / TypeScript Log Reading Example

Using the napi-rs binding wrapping `gix::Repository`:

```ts
import { Repository } from "./dist/index.js";

const repo = Repository.open(process.cwd());

// Read commit history starting at HEAD (limit 10 commits)
const logs = repo.log("HEAD", 10, false);

for (const entry of logs) {
  console.log(`Commit: ${entry.id}`);
  console.log(`Author: ${entry.authorName} <${entry.authorEmail}>`);
  console.log(`Subject: ${entry.subject}`);
  console.log(`Message: ${entry.message}`);
  console.log(`Tree: ${entry.treeId}`);
  console.log(`Parents: ${entry.parentIds.join(", ")}`);
  console.log("---");
}
```

### 3. References & HEAD

- `r.head()` -> `gix::Head`: Access `HEAD` reference (symbolic or detached).
- `r.head_commit()` -> `gix::Commit`: Directly obtain `HEAD`'s pointed commit.
- `r.head_id()` -> `gix::ObjectId`: Obtain `HEAD`'s pointed OID.
- `r.references()`: Iterator over all references in the repository.
- `r.find_reference(name)`: Find a reference by name (e.g. `refs/heads/main`).
- `r.reference(name, target, constraint, log_message)`: Create or update a reference.
- `r.tag(...)` / `r.tag_reference(...)`: Create annotated or lightweight tags.

### 4. Commit Verification & Signing

- `commit.verify_signature()`: Verifies cryptographic signatures (`OpenPGP`, `SSH`, `X509`) attached to commits using repository-configured programs (`gpg`, `ssh-keygen`, `gpgsm`).
- `commit.sign(options)`: Generates and attaches a signature to commit data using `gix::commit::sign::Options`.

### 5. Remote Configuration & Operations

- `r.remote_names()`: List all configured remote names.
- `r.find_remote(name)`: Find a configured remote by name (`origin`, etc.).
- `r.remote_at_url(url, direction)`: Construct an anonymous remote at a given URL.
- `remote.connect(direction)` -> `prepare_fetch(...)` -> `receive(...)`: Perform network operations such as fetch.
