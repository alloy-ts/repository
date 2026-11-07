# ObjectStore API

Low-level, repository-independent object databases. Where `Repository` bundles
the object database with refs, the index, and the worktree, these classes
operate directly on an `objects/` directory and expose gix's object stores
without any higher-level machinery.

There are two flavors:

- **`ObjectStore`** — wraps `gix::odb::loose::Store`. Reads and writes **loose**
  objects only (one file per object). It does **not** consult packs, alternates,
  or the rest of a repository. Use it for content-addressed scratch storage,
  testing, or when you know your objects are loose.
- **`ObjectDatabase`** — wraps `gix::odb::Cache<Handle<Arc<gix::odb::Store>>>`.
  The full, pack-aware object database: it reads loose objects **and** packs
  (including multi-pack indexes) and honors alternates. It also layers gix's
  pack/object caches on top for repeated or delta-chain decodes. `into_inner()`
  drops the cache and exposes the bare `ObjectHandle`.

All object ids are lowercase 40-character SHA-1 hex strings. Methods marked
`null` return it when an object is missing or (where noted) when the id is
malformed.

## ObjectStore

### Constructor

| Signature                                                                                        | Notes                                                                                                                                                                              |
| ------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `new ObjectStore(objects_directory: string, object_hash?: string, options?: ObjectStoreOptions)` | Opens the loose database rooted at `objects_directory` (typically `<repo>/.git/objects`). The directory **must already exist**. `object_hash` is `"sha1"` (only sha1 is built in). |

```ts
interface ObjectStoreOptions {
  alloc_limit_bytes?: number; // cap a single allocation from a loose object read/write
  compression_level?: number; // zlib level 0–9; default = git's BEST_SPEED (level 1)
}
```

### Methods

| Method         | Signature                                | Notes                                                                                                                                        |
| -------------- | ---------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| `contains`     | `(id: string) => boolean`                | `false` for malformed ids.                                                                                                                   |
| `lookupPrefix` | `(prefix: string) => PrefixLookup`       | Throws on malformed hex; returns `found: false` when no object matches.                                                                      |
| `tryFind`      | `(id: string) => ObjectData \| null`     | `{ kind, data }` — raw, decompressed body without the loose-object header. `null` when missing or malformed.                                 |
| `tryHeader`    | `(id: string) => ObjectHeader \| null`   | `{ kind, size }` — body size in bytes; cheaper than `tryFind`. `null` when missing or malformed.                                             |
| `writeObject`  | `(kind: string, data: Buffer) => string` | `kind` ∈ `commit`/`tree`/`blob`/`tag`. Writes raw bytes as-is (inverse of `tryFind`). Returns the id. Throws on invalid `kind` or I/O error. |
| `objectPath`   | `(id: string) => string`                 | Absolute path of the sharded loose file; throws on a malformed id. May not exist yet.                                                        |

Getters: `path: string` (the `objects` directory), `object_hash: string`
(`"sha1"`).

## ObjectDatabase

### Constructor

| Signature                                                                        | Notes                                                                                                                                                               |
| -------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `new ObjectDatabase(objects_directory: string, options?: ObjectDatabaseOptions)` | Opens the pack-aware store at `objects_directory` and wraps it in gix's cache. The directory must already exist. `object_hash` is `"sha1"` (only sha1 is built in). |

```ts
interface ObjectDatabaseOptions {
  object_hash?: string; // "sha1" (default)
  slots?: number; // fixed number of index/pack slots; default derived from disk state
  use_multi_pack_index?: boolean; // default true
  alloc_limit_bytes?: number; // cap a single allocation from on-disk pack data
  loose_compression?: number; // zlib level 0–9 for loose writes; default BEST_SPEED
  pack_cache?: string; // "none" installs gix's Never pack cache
  object_cache?: string; // "none" installs gix's Never object cache
}
```

> Only the `"none"` cache kind is available without enabling the gix
> `pack-cache-lru-*` feature. An LRU cache would be installed via the same
> setter with a different kind once the feature is on.

### Cache management

| Method               | Signature                | Notes                                                                      |
| -------------------- | ------------------------ | -------------------------------------------------------------------------- |
| `has_object_cache`   | `() => boolean`          | Whether an object cache is installed.                                      |
| `has_pack_cache`     | `() => boolean`          | Whether a pack cache is installed.                                         |
| `setPackCache`       | `(kind: string) => void` | Install/replace the pack cache; throws on an unsupported `kind`.           |
| `setObjectCache`     | `(kind: string) => void` | Install/replace the object cache; throws on an unsupported `kind`.         |
| `unset_pack_cache`   | `() => void`             | Remove the pack cache (and its constructor).                               |
| `unset_object_cache` | `() => void`             | Remove the object cache (and its constructor).                             |
| `into_inner`         | `() => ObjectHandle`     | Dissolve the cache and return the bare handle.                             |
| `into_arc`           | `() => ObjectDatabase`   | No-op for the already-`Arc`-backed handle; returns an equivalent database. |

Getters: `path: string`, `object_hash: string`.

### Thread-safety note

The underlying `Cache` and `Handle` hold `RefCell`s and are not `Sync`, so they
are kept behind a `Mutex` in this binding. Because all synchronous NAPI calls
run on the JavaScript main thread, the installed caches **do** persist across
calls and speed up repeated lookups within a process — they are simply not
shared across threads.

## ObjectHandle

Obtained from `ObjectDatabase.into_inner()`. The bare, pack-aware handle (no
caches).

| Method             | Signature                              | Notes                                                                        |
| ------------------ | -------------------------------------- | ---------------------------------------------------------------------------- |
| `contains`         | `(id: string) => boolean`              | Loose **or** packed. `false` for malformed ids.                              |
| `lookupPrefix`     | `(prefix: string) => PrefixLookup`     | Throws on malformed hex; `found: false` when no object matches.              |
| `tryFind`          | `(id: string) => ObjectData \| null`   | Loose **or** packed. `null` when missing or malformed.                       |
| `tryHeader`        | `(id: string) => ObjectHeader \| null` | Loose **or** packed. `null` when missing or malformed.                       |
| `structure`        | `() => StoreRecord[]`                  | One record per loose db / index / multi-index; see below.                    |
| `alternateDbPaths` | `() => string[]`                       | `objects` directories contributed by alternates (empty when there are none). |

Getters: `path: string`, `object_hash: string`.

## Shared types

```ts
interface ObjectData {
  kind: string; // "commit" | "tree" | "blob" | "tag"
  data: Buffer; // raw, decompressed body (no loose-object header)
}

interface ObjectHeader {
  kind: string; // "commit" | "tree" | "blob" | "tag"
  size: number; // encoded body size in bytes
}

interface PrefixLookup {
  found: boolean; // true iff exactly one object matches the prefix
  ambiguous: boolean; // true iff more than one object matches
  id?: string; // present iff found === true
}

interface StoreRecord {
  kind: string; // "loose" | "index" | "multi_index" | "empty"
  path?: string; // loose db / index / multi-index path (absent for "empty")
  num_objects?: number; // loose object count (loose records only)
  state?: string; // "loaded" | "disposable" | "unloaded" (index/multi_index only)
}
```

## Working example

```ts
import { ObjectDatabase, ObjectHandle, ObjectStore } from "@alloy-ts/repository";
import { execSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

// --- Loose store: content-addressed scratch storage -------------------------
const tmp = mkdtempSync(join(tmpdir(), "loose-"));
const looseDir = join(tmp, "objects");
mkdirSync(looseDir, { recursive: true });
const loose = new ObjectStore(looseDir);

const blobId = loose.writeObject("blob", Buffer.from("hello\n"));
console.log(loose.contains(blobId)); // true
console.log(loose.tryHeader(blobId)); // { kind: "blob", size: 6 }
console.log(loose.tryFind(blobId)!.data.toString()); // "hello\n"
console.log(loose.lookupPrefix(blobId.slice(0, 12)).id === blobId); // true

// --- Pack-aware database: loose + packs + alternates ------------------------
const objectsDir = join(
  process.cwd(),
  execSync("git rev-parse --git-common-dir").toString().trim(),
  "objects",
);
const db = new ObjectDatabase(objectsDir);
const head = execSync("git rev-parse HEAD").toString().trim();

console.log(db.contains(head)); // true (may be packed)
const commit = db.tryFind(head)!;
console.log(commit.kind); // "commit"
console.log(commit.data.length === db.tryHeader(head)!.size); // true

// Toggle caches, then drop them and keep the bare handle.
db.setPackCache("none");
console.log(db.has_pack_cache()); // true
db.unset_pack_cache();
const handle: ObjectHandle = db.into_inner();
console.log(handle.structure().length > 0); // true
console.log(handle.alternateDbPaths()); // string[]

rmSync(tmp, { recursive: true, force: true });
```

## Error behavior

- `ObjectStore`/`ObjectHandle` lookup methods that return
  `ObjectData`/`ObjectHeader` (`tryFind`, `tryHeader`) return `null` when the
  object is missing **or** the id is malformed. `contains` returns `false` for
  malformed ids.
- `lookupPrefix` **throws** on malformed hex input; it returns `found: false`
  (`ambiguous: false`) when no object matches the (valid) prefix, and
  `ambiguous: true` when more than one matches.
- `objectPath` throws on a malformed id (it only computes a path); `writeObject`
  throws on an invalid `kind` or an I/O error.
- Constructors throw when `objects_directory` is missing/not a directory, or
  when an unsupported `object_hash` (e.g. `"sha256"`, which needs the gix sha256
  feature) is given.
- `setPackCache`/`setObjectCache` throw on an unsupported cache `kind`.
- `structure`/`alternateDbPaths` throw on I/O errors (e.g. an unreadable object
  database).
