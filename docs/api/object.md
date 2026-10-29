# Object

Everything that reads, writes, or decodes git objects: the raw object store, and
the typed wrappers for blobs, commits, tags, and trees.

All object ids are lowercase 40-character SHA-1 hex strings (matching
`repo.objectHash()`). Methods that look objects up by id throw when the object
is missing, unless documented as returning `null`.

## Object ids

Object ids are plain hex strings — 40 characters for SHA-1, 64 for SHA-256
repositories. For a first-class wrapper there is the `Oid` class:

```ts
const oid = new Oid(repo.headId()); // parse hex; throws on abbreviations/non-hex
oid.hex; // "c859dbe6…" — lowercase hex
String(oid); // hex, so template literals work
JSON.stringify(oid); // '"c859dbe6…"' — toJSON returns the hex id
oid.format(); // "sha1" | "sha256"
oid.isZero(); // false
oid.toBytes(); // Buffer — raw 20/32-byte digest
Oid.fromBytes(oid.toBytes()).equals(oid); // true
oid.compare(Oid.zero()); // 1 — byte-wise (git_oid_cmp); -1/0/1
Oid.zero("sha256").hex.length; // 64
```

`Oid` is a convenience wrapper: repository methods still take hex strings, so
pass `oid.hex` (or `String(oid)`) wherever an id is expected.

The free functions (no repository needed) complement it:

| Function       | Signature                                    | Notes                                                                                             |
| -------------- | -------------------------------------------- | ------------------------------------------------------------------------------------------------- |
| `oidIsValid`   | `(id: string, format?: string) => boolean`   | Strict: full digest length only; with `format` the length must match. Abbreviations are not oids. |
| `oidFormat`    | `(id: string) => "sha1" \| "sha256" \| null` | Inferred from the hex length; `null` when `id` is not a valid object id.                          |
| `oidIsZero`    | `(id: string) => boolean`                    | All-zero id check (`git_oid_is_zero`); `false` for invalid strings.                               |
| `oidZero`      | `(format?: string) => string`                | The all-zero id (40 or 64 `0`s); libgit2's `Oid::ZERO_SHA1`/`Oid::ZERO_SHA256`.                   |
| `oidToBytes`   | `(id: string) => Buffer`                     | Raw digest bytes (20/32); throws on invalid ids.                                                  |
| `oidFromBytes` | `(bytes: Buffer) => string`                  | Hex id of a raw digest; throws unless `bytes` is 20 or 32 bytes.                                  |

The `Oid` class (`new Oid(hex)`, `Oid.fromBytes(bytes)`, `Oid.zero(format?)`)
exposes the same checks as methods — `hex`, `format()`, `isZero()`, `toBytes()`,
`equals(other)`, `compare(other)` — plus `toString()`/`toJSON()` returning the
hex id.

Every entry point that takes a hash-kind string (`ObjectStore`'s constructor,
`Graph.at`, `oidIsValid`, `oidZero`) accepts any casing — `"sha1"`, `"Sha1"`,
`"sha256"`, `"Sha256"`. `Repository.objectHash()` still returns the `Debug`
casing (`"Sha1"`).

```ts
oidIsValid(repo.headId()); // true
oidIsValid(repo.headId().slice(0, 7)); // false — prefixes are resolved via revParseSingle
oidFormat(repo.headId()); // "sha1"
oidToBytes(repo.headId()).length; // 20
```

## Raw object store

Raw methods treat objects as opaque bytes of a known kind — no decoding happens.

### `Repository`

| Method            | Signature                                | Notes                                                                                                                        |
| ----------------- | ---------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `findObject`      | `(id: string) => GitObject`              | Throws when the object is missing.                                                                                           |
| `tryFindObject`   | `(id: string) => GitObject \| null`      | `null` when missing or when `id` is not a valid hex id.                                                                      |
| `objectExists`    | `(id: string) => boolean`                | `false` for malformed ids.                                                                                                   |
| `readObject`      | `(id: string) => ObjectData`             | `{ kind: string, data: Buffer }` — the raw, decompressed body without the loose-object header.                               |
| `writeObject`     | `(kind: string, data: Buffer) => string` | `kind` is one of `commit`, `tree`, `blob`, `tag`. Writes raw bytes as-is (inverse of `readObject`). Returns the id.          |
| `computeHash`     | `(kind: string, data: Buffer) => string` | Computes the id without writing anything to the object database.                                                             |
| `computeHashFile` | `(kind: string, path: string) => string` | Computes the id of the file at `path` without writing anything (libgit2's `Oid::hash_file`); streams, so files may be large. |
| `findHeader`      | `(id: string) => ObjectHeader`           | `{ kind, size, isPacked, numDeltas }` — size of the encoded body in bytes; cheaper than `readObject`. Throws when missing.   |
| `tryFindHeader`   | `(id: string) => ObjectHeader \| null`   | `null` when missing or malformed.                                                                                            |
| `peel`            | `(id: string, kind: string) => string`   | Follows tag→commit→tree chains until an object of `kind` is reached; returns its id. Throws if that kind cannot be reached.  |

`ObjectHeader` reports where the object was found: `isPacked` is `true` for
objects that came out of a pack (with `numDeltas` counting the deltas decoded)
and `false` for loose objects (where `numDeltas` is `undefined`).

### `GitObject`

A kinded wrapper around any object, with downcast helpers:

```ts
const obj = repo.findObject(commitId);
obj.kind; // "commit" | "tree" | "blob" | "tag"
obj.id; // hex id
obj.data(); // Buffer — raw body
obj.shortId(); // "a1b2c3d" — shortest unambiguous abbreviation (core.abbrev aware)

obj.asCommit(); // Commit | null
obj.asTree(); // Tree | null
obj.asBlob(); // Blob | null
obj.asTag(); // Tag | null

obj.peelToCommit(); // Commit — follows tags until a commit
obj.peelToTree(); // Tree — follows tags/commits until a tree
obj.peelToBlob(); // Blob
obj.peelToTag(); // Tag
obj.peelToKind("tree"); // GitObject — peel to any of "commit" | "tree" | "blob" | "tag" | "any"
obj.peelToEnd(); // GitObject — follows tag chains to the first non-tag object

obj.findHeader(); // ObjectHeader — kind/size/isPacked without decoding
obj.describe({ idAsFallback: true }); // DescribeOutcome | null — `git describe`
```

Downcasts check the object's kind and return `null` on mismatch. The well-known
empty tree id is resolvable via `findTree`/`findObject` even when the object is
not stored in the database (`git` itself leaves it out of most repositories).

### Peeling

`peelToKind("any")` and `peelToEnd()` are the `git rev-parse <rev>^{}`
operation: they follow tag chains only, so a tag on a commit yields that
_commit_, never its tree. The other `peelToX` methods peel as far as needed —
`peelToTree()` walks a tag→commit→tree chain to the end. Peeling throws when the
chain cannot reach the requested kind (e.g. `peelToCommit()` on a tag pointing
at a blob).

### Object cache

The in-memory object cache only affects the current synchronous call, since each
call gets a fresh thread-local repository view.

| Method                               | Signature                 | Notes                                                         |
| ------------------------------------ | ------------------------- | ------------------------------------------------------------- |
| `objectCacheSize`                    | `(bytes: number) => void` | Set the cache size in bytes; `0` disables.                    |
| `objectCacheSizeIfUnset`             | `(bytes: number) => void` | Only applies when not set already.                            |
| `computeObjectCacheSizeForTreeDiffs` | `() => number`            | Recommended size for tree-diff workloads, based on the index. |
| `withObjectMemory`                   | `() => void`              | Keep decompressed objects in memory for the view's lifetime.  |

## Blobs

File contents, stored verbatim.

### `Repository`

| Method            | Signature                  | Notes                                                                             |
| ----------------- | -------------------------- | --------------------------------------------------------------------------------- |
| `findBlob`        | `(id: string) => Blob`     | Throws when missing or not a blob.                                                |
| `writeBlob`       | `(data: Buffer) => string` | Writes the blob and returns its id. Duplicate objects are pre-hashed and skipped. |
| `writeBlobStream` | `(data: Buffer) => string` | Same result; writes through the streaming path.                                   |

### `Blob`

```ts
const id = repo.writeBlob(Buffer.from("hello\n"));
const blob = repo.findBlob(id);
blob.id; // hex id
blob.content(); // Buffer
```

## Commits

### `Repository`

| Method        | Signature                                                                                                                  | Notes                                                                                                                                                                                                                   |
| ------------- | -------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `headId`      | `() => string`                                                                                                             | The id that `HEAD` resolves to.                                                                                                                                                                                         |
| `headCommit`  | `() => Commit`                                                                                                             | Throws if `HEAD` is unborn.                                                                                                                                                                                             |
| `head`        | `() => Reference`                                                                                                          | The `HEAD` reference itself: symbolic when `HEAD` points at a branch, direct when detached. Inspect `reference.name`, `reference.symbolicTarget`, `reference.target`, or call `reference.peelToId()` for the commit id. |
| `findCommit`  | `(id: string) => Commit`                                                                                                   | Throws when missing or not a commit.                                                                                                                                                                                    |
| `writeCommit` | `(input: CommitInput) => string`                                                                                           | Writes a commit object directly to the ODB **without moving any reference**; returns the id. Author/committer fall back to the repository identity when omitted.                                                        |
| `commit`      | `(reference: string, message: string, tree: string, parents: string[]) => string`                                          | Creates a commit and advances `reference`; returns the id.                                                                                                                                                              |
| `commitAs`    | `(committer: Signature, author: Signature, reference: string, message: string, tree: string, parents: string[]) => string` | Same, with explicit identities and times.                                                                                                                                                                               |
| `newCommit`   | `(message: string, tree: string, parents: string[]) => string`                                                             | Writes the commit without updating any reference.                                                                                                                                                                       |
| `newCommitAs` | `(committer: Signature, author: Signature, message: string, tree: string, parents: string[]) => string`                    | Same, with explicit identities and times.                                                                                                                                                                               |

```ts
interface CommitInput {
  tree: string; // root tree id
  parents: string[]; // parent commit ids
  message: string;
  author?: Signature; // defaults to the configured repository identity
  committer?: Signature;
}

interface Signature {
  name: string;
  email: string;
  time: number; // unix seconds
  timezoneOffset: number; // minutes
}
```

### `Commit`

```ts
const commit = repo.headCommit();
commit.id; // hex id
commit.message; // full message
commit.treeId; // root tree id
commit.parentIds; // string[]
commit.author(); // Signature
commit.committer(); // Signature
commit.encoding; // string | null — the `encoding` header; assume UTF-8 when null
commit.extraHeaders(); // CommitHeaderEntry[] — custom headers beyond the standard set
commit.signature; // string | null — raw `gpgsig` header (carries ≠ valid)
commit.verify(); // VerifyOutcome | null — shells out to GPG/SSH; slow
commit.sign(); // Commit — new object carrying a signature per repo config
```

`CommitHeaderEntry` is `{ name: string, value: string }` in header order.
`VerifyOutcome` contains `status`, `trustLevel`, and `signer` among other
fields; `null` means unsigned.

## Tags

Annotated tag objects (`git tag -a`). Lightweight tags are references, not
objects.

### `Repository`

| Method      | Signature                                                                       | Notes                                                                                                      |
| ----------- | ------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------- |
| `findTag`   | `(id: string) => Tag`                                                           | Throws when missing or not a tag object.                                                                   |
| `createTag` | `(name: string, target: string, message: string, force?: boolean) => Reference` | Creates the tag object **and** the `refs/tags/<name>` reference. `force` overwrites an existing reference. |

### `Tag`

```ts
const ref = repo.createTag("v1.0.0", repo.headId(), "First release\n");
const tag = repo.findTag(ref.targetId);
tag.id; // hex id of the tag object
tag.name; // "v1.0.0"
tag.message; // full message
tag.targetId; // hex id of the tagged object
tag.targetKind; // "commit" | "tree" | "blob" | "tag"
tag.tagger(); // Signature | null
tag.signature; // string | null — armored in-body signature (carries ≠ valid)
tag.sign(); // Tag — new object signed per repo config
tag.verify(); // VerifyOutcome | null — shells out to GPG/SSH; slow
```

## Trees

Directory snapshots. Entries point at blobs, subtrees (`mode "040000"`),
symlinks (`"120000"`), or submodules (`"160000"`).

# Tree

Directory snapshots: reading, walking, authoring and canonicalising git trees.

A `Tree` is a content-addressed **prolly tree** and compatible with a **git
tree**. They expose the same API. Data is stored in a content-addressed
`FileNodeStore` holding the canonical tree structure — the flat
`path -> "mode oid"` leaves, and also surfaced to the git object database as
well. Identical content always yields an identical 64-hex root. Their reads are
served straight from the flat prolly map — no gix tree decode — and their git
tree oid is _derived_ by serialising the tree and hashing it, without touching
the git object database.

See [Tree](tree.md) for the full reference (prolly-backed reads, id derivation,
and the `TreeBuilder`).

### `Repository`

| Method               | Signature                                                | Notes                                                                                                                                                                                |
| -------------------- | -------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `findTree`           | `(id: string) => Tree`                                   | Throws when missing or not a tree.                                                                                                                                                   |
| `writeTree`          | `(entries: TreeItemInput[]) => string`                   | Builds a tree object from explicit entries; returns the id.                                                                                                                          |
| `editTree`           | `(baseTreeId: string, edits: TreeEditInput[]) => string` | Applies upserts/removals on top of a base tree; returns the new tree id. Intermediate trees along nested paths are created automatically; now-empty subtrees are pruned, like `git`. |
| `emptyTree`          | `() => Tree`                                             | The well-known empty tree (`4b825dc6…`), even when it is not in the database.                                                                                                        |
| `prollyTree`         | `(root: string) => Tree`                                 | Opens the prolly-backed tree for a prolly root (64-hex).                                                                                                                             |
| `prollyTreeFromTree` | `(treeId: string) => GenericTreeRef`                     | Flattens a single git tree into the prolly store; returns `{ root, nodeStore }`.                                                                                                     |

```ts
interface TreeItemInput {
  path: string; // single-level file name
  mode?: string; // "100644" (default), "100755", "040000", "120000", "160000"
  id: string; // target object id
}

interface TreeEditInput {
  path: string; // nested paths allowed, e.g. "a/b/c.txt"
  mode?: string; // for upserts; defaults to "100644"
  id?: string; // required for upserts
  remove?: boolean;
  removeMode?: "any" | "leaf" | "ifLeaf";
  // "any" (default): remove the entry, pruning empty subtrees below it
  // "leaf": error when the entry at `path` is a tree
  // "ifLeaf": leave an existing tree untouched instead of removing it
}
```

### `Tree` and entries

```ts
const tree = repo.findTree(repo.headCommit().treeId);

tree.id; // hex id
tree.entries; // TreeEntry[] — all direct entries
tree.entry("file.txt"); // TreeEntry | null — single-level lookup by name
tree.entryByPath("a/b/c.txt"); // Entry | null — nested lookup, follows subtrees
tree.canonicalise(); // 64-hex prolly root — the canonical form of this tree
```

```ts
interface TreeEntry {
  mode: string; // "100644", "100755", "040000", "120000", "160000"
  filename: string; // single path component
  id: string; // target object id
}
```

`Entry` (returned by `entryByPath`) carries the same data plus repository access
for traversal:

```ts
const entry = tree.entryByPath("a/b/c.txt");
entry.id; // hex id of the blob
entry.filename; // "c.txt" — last path component
entry.mode; // "100644"
entry.treeId; // id of the tree it was resolved from
entry.isTree(); // true for directories ("040000")
entry.asTree(); // Tree | null — resolve directory entries for further traversal
```

### Prolly-backed trees

```ts
// Canonicalise a git tree into the repository's prolly node store.
const root = repo.findTree(repo.headCommit().treeId).canonicalise(); // 64-hex

// Rebuild the tree from the prolly root alone — same entries, same derived git id.
const tree = repo.prollyTree(root);
tree.id === repo.findTree(repo.headCommit().treeId).id; // true
```

Prolly-backed trees answer `entries`, `len`, `get`, `getById`, `entry`,
`entryByPath`, `walk` and `intoEntries` from the flat prolly map — no gix tree
decode, no object-database lookup. Directory entries carry mode `"040000"` and a
derived git tree oid.

Authoring is prolly-native too: `TreeBuilder` buffers edits in a
content-addressed prolly tree and `write()` canonicalises the result directly,
without building a git tree. See the [Trees API](tree.md).

## Working example

```ts
import { Repository } from "@alloy-ts/repository";

const repo = Repository.open("/path/to/repo");

// Read the file at HEAD without touching the worktree
const head = repo.headCommit();
const entry = repo.findTree(head.treeId).entryByPath("src/main.ts");
const content = entry ? repo.findBlob(entry.id).content() : null;

// Build a tree, commit it, tag it
const blobId = repo.writeBlob(Buffer.from("v2 content\n"));
const treeId = repo.editTree(head.treeId, [
  {
    path: "src/main.ts",
    mode: "100644",
    id: blobId,
  },
]);
const commitId = repo.commit("HEAD", "Update main.ts", treeId, [head.id]);
repo.createTag("v2.0.0", commitId, {
  message: "Second release\n",
  force: true,
});

// Round-trip raw objects
const raw = repo.readObject(treeId);
const sameId = repo.writeObject(raw.kind, raw.data);
console.log(sameId === treeId); // true
```

## Error behavior

- Lookup methods (`findObject`, `findCommit`, `findTree`, `findTag`, `findBlob`,
  `findHeader`) throw on a missing object **or** a kind mismatch.
- `try*` variants (`tryFindObject`, `tryFindHeader`) return `null` instead,
  including for malformed hex ids.
- `objectExists` returns `false` for malformed ids; `writeObject`/`computeHash`
  throw on an invalid `kind`.
- The `oid*` helpers throw only where documented (`oidToBytes`/`oidFromBytes`);
  `oidIsValid`/`oidFormat`/`oidIsZero` return `false`/`null` for invalid input,
  and `oidZero` throws on an unknown hash kind.
- `computeHashFile` throws when the file cannot be opened or read; it never
  writes to the object database.
- Peeling throws when the requested kind is unreachable, and `peelToKind` throws
  on an unknown kind string (`invalid object kind bogus`).
- `shortId()` falls back to the full 40-hex id when the prefix cannot be
  shortened, so it never throws for a valid handle.
- `describe()` throws when the object is not a commit; it returns `null` when no
  candidate name is found and `idAsFallback` is off.
- `editTree` throws on an invalid `mode`, an invalid `removeMode`, a missing
  `id` for an upsert, or `removeMode: "leaf"` when the target is a tree.
