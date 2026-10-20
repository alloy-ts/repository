# Objects API

Everything that reads, writes, or decodes git objects: the raw object store, and
the typed wrappers for blobs, commits, tags, and trees.

All object ids are lowercase 40-character SHA-1 hex strings (matching
`repo.objectHash()`). Methods that look objects up by id throw when the object
is missing, unless documented as returning `null`.

## Raw object store

Raw methods treat objects as opaque bytes of a known kind — no decoding happens.

### `Repository`

| Method          | Signature                                | Notes                                                                                                                       |
| --------------- | ---------------------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| `findObject`    | `(id: string) => GitObject`              | Throws when the object is missing.                                                                                          |
| `tryFindObject` | `(id: string) => GitObject \| null`      | `null` when missing or when `id` is not a valid hex id.                                                                     |
| `objectExists`  | `(id: string) => boolean`                | `false` for malformed ids.                                                                                                  |
| `readObject`    | `(id: string) => ObjectData`             | `{ kind: string, data: Buffer }` — the raw, decompressed body without the loose-object header.                              |
| `writeObject`   | `(kind: string, data: Buffer) => string` | `kind` is one of `commit`, `tree`, `blob`, `tag`. Writes raw bytes as-is (inverse of `readObject`). Returns the id.         |
| `computeHash`   | `(kind: string, data: Buffer) => string` | Computes the id without writing anything to the object database.                                                            |
| `findHeader`    | `(id: string) => ObjectHeader`           | `{ kind: string, size: number }` — size of the encoded body in bytes; cheaper than `readObject`. Throws when missing.       |
| `tryFindHeader` | `(id: string) => ObjectHeader \| null`   | `null` when missing or malformed.                                                                                           |
| `peel`          | `(id: string, kind: string) => string`   | Follows tag→commit→tree chains until an object of `kind` is reached; returns its id. Throws if that kind cannot be reached. |

### `GitObject`

A kinded wrapper around any object, with downcast helpers:

```ts
const obj = repo.findObject(commitId);
obj.kind; // "commit" | "tree" | "blob" | "tag"
obj.id; // hex id
obj.data(); // Buffer — raw body

obj.asCommit(); // Commit | null
obj.asTree(); // Tree | null
obj.asBlob(); // Blob | null
obj.asTag(); // Tag | null

obj.peelToCommit(); // Commit — follows tags until a commit
obj.peelToTree(); // Tree — follows tags/commits until a tree
```

Downcasts check the object's kind and return `null` on mismatch. The well-known
empty tree id is resolvable via `findTree`/`findObject` even when the object is
not stored in the database (`git` itself leaves it out of most repositories).

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

| Method        | Signature                                                                                                                  | Notes                                                                                                                                                            |
| ------------- | -------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `headId`      | `() => string`                                                                                                             | The id that `HEAD` resolves to.                                                                                                                                  |
| `headCommit`  | `() => Commit`                                                                                                             | Throws if `HEAD` is unborn.                                                                                                                                      |
| `findCommit`  | `(id: string) => Commit`                                                                                                   | Throws when missing or not a commit.                                                                                                                             |
| `writeCommit` | `(input: CommitInput) => string`                                                                                           | Writes a commit object directly to the ODB **without moving any reference**; returns the id. Author/committer fall back to the repository identity when omitted. |
| `commit`      | `(reference: string, message: string, tree: string, parents: string[]) => string`                                          | Creates a commit and advances `reference`; returns the id.                                                                                                       |
| `commitAs`    | `(committer: Signature, author: Signature, reference: string, message: string, tree: string, parents: string[]) => string` | Same, with explicit identities and times.                                                                                                                        |
| `newCommit`   | `(message: string, tree: string, parents: string[]) => string`                                                             | Writes the commit without updating any reference.                                                                                                                |
| `newCommitAs` | `(committer: Signature, author: Signature, message: string, tree: string, parents: string[]) => string`                    | Same, with explicit identities and times.                                                                                                                        |

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

### `Repository`

| Method      | Signature                                                | Notes                                                                                                                                                                                |
| ----------- | -------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `findTree`  | `(id: string) => Tree`                                   | Throws when missing or not a tree.                                                                                                                                                   |
| `writeTree` | `(entries: TreeItemInput[]) => string`                   | Builds a tree object from explicit entries; returns the id.                                                                                                                          |
| `editTree`  | `(baseTreeId: string, edits: TreeEditInput[]) => string` | Applies upserts/removals on top of a base tree; returns the new tree id. Intermediate trees along nested paths are created automatically; now-empty subtrees are pruned, like `git`. |
| `emptyTree` | `() => Tree`                                             | The well-known empty tree (`4b825dc6…`), even when it is not in the database.                                                                                                        |

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
repo.createTag("v2.0.0", commitId, "Second release\n", true);

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
- `editTree` throws on an invalid `mode`, an invalid `removeMode`, a missing
  `id` for an upsert, or `removeMode: "leaf"` when the target is a tree.
