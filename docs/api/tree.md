# Tree

Directory snapshots: reading, walking, authoring and canonicalising git trees.

A `Tree` is **prolly-backed** (a content-addressed prolly tree root) and
**git-compatible** (compatible with a git tree) expose the same API. The nodes
are stored in a content-addressed `FileNodeStore` holding the canonical tree
structure — the flat `path -> "mode oid"` leaves, and also surfaced to the git
object database as well.

## Prolly form

The prolly form is the **canonical** representation ("prolly canonical, git
derived"): the tree's content is a prolly tree whose leaves are the flattened
entries, keyed by path.

```text
path            ->  "100644 3f2a…"
```

The leaf value is the entry mode and the **git** object id of the blob. The
prolly root is a 64-hex SHA-256 content id (`Cid`), so identical tree content
always yields an identical root — no matter how the entries were created or
ordered.

A prolly-backed tree is created either by canonicalising a git tree or by
writing one with a `TreeBuilder`:

```ts
const tree = repo.findTree(repo.headCommit().treeId);
const root = tree.canonicalise(); // 64-hex prolly root

// Rebuild the tree from the prolly root alone.
const same = repo.prollyTree(root);
same.id === tree.id; // true — the git oid is derived, and matches
same.canonicalise() === root; // true — canonicalise short-circuits
```

### Reads are served from the flat map

For a prolly-backed tree these are answered from the flat prolly map — there is
no gix tree decode and no object-database lookup:

`entries`, `len`, `isEmpty`, `get`, `getById`, `entry`, `entryByPath`, `walk`,
`intoEntries`.

Leaf ids are the real blob ids, read straight off the map. Directory entries are
advertised with mode `"040000"` and a **derived** git tree oid, computed by
serializing that subtree and hashing it.

### The git id is derived, not stored

`tree.id` is the git tree oid. For a prolly-backed tree it is _derived_ on
demand: the tree bytes are serialized and hashed. Nothing is written to the
object database by that derivation.

The encoder is a self-contained implementation of git's tree format:

- **Ordering** — git's `name_compare`: names compare byte-wise, but a directory
  is treated as if its name ended with `/`. This is what makes a directory `a`
  sort before a sibling file `a.txt`.
- **Bytes** — `<octal-mode> <name>\0<raw oid bytes>` per entry, with the modes
  `100644`, `100755`, `40000`, `120000`, `160000`.

So a prolly-backed tree's id is byte-identical to the id of the equivalent git
tree.

## Repository

| Method               | Signature                                                | Notes                                                                                                                                                                                |
| -------------------- | -------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `findTree`           | `(id: string) => Tree`                                   | Throws when missing or not a tree.                                                                                                                                                   |
| `emptyTree`          | `() => Tree`                                             | The well-known empty tree (`4b825dc6…`), even when it is not in the database.                                                                                                        |
| `writeTree`          | `(entries: TreeItemInput[]) => string`                   | Builds a tree object from explicit entries; returns the id.                                                                                                                          |
| `editTree`           | `(baseTreeId: string, edits: TreeEditInput[]) => string` | Applies upserts/removals on top of a base tree; returns the new tree id. Intermediate trees along nested paths are created automatically; now-empty subtrees are pruned, like `git`. |
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

## `Tree`

```ts
const tree = repo.findTree(repo.headCommit().treeId);

tree.id; // hex id
tree.entries; // TreeEntry[] — all direct entries
tree.len(); // number — entry count
tree.isEmpty; // boolean
tree.get(0); // TreeEntry | null — by index
tree.getById(id); // TreeEntry | null — by target object id
tree.entry("file.txt"); // TreeEntry | null — single-level lookup by name
tree.entryByPath("a/b/c.txt"); // Entry | null — nested lookup, follows subtrees
tree.intoEntries(); // Entry[] — live Entry objects, each with treeId set
tree.walk(); // TreeWalkEntry[] — recursive, every entry with its full path
tree.walk({ contents: true }); // …including each blob's raw content
tree.asObject(); // Object — the tree as a git object
tree.canonicalise(); // 64-hex prolly root
```

Constructing trees:

```ts
const tree = new Tree(entries); // from Entry objects
Tree.fromEntries(entries); // same, as a factory
Tree.fromContents(repo, [{ path: "src/main.ts", content: "hello\n" }]); // writes the blobs
```

`Tree.fromContents` is the content-oriented counterpart to `Tree.fromEntries`:
it writes each `content` as a blob and creates the intermediate directories.
`mode` defaults to `"100644"`.

### `TreeEntry`

```ts
interface TreeEntry {
  mode: string; // "100644", "100755", "040000", "120000", "160000"
  filename: string; // single path component
  id: string; // target object id
}
```

`TreeEntry` also carries helpers:

```ts
entry.kind; // "blob" | "blobExecutable" | "tree" | "link" | "commit" | "unknown"
entry.toBlob(repo); // Blob | null
entry.toTree(repo); // Tree | null
```

`Entry` (returned by `entryByPath` and `intoEntries`) carries the same data plus
repository access for traversal:

```ts
const entry = tree.entryByPath("a/b/c.txt");
entry.id; // hex id of the blob
entry.filename; // "c.txt" — last path component
entry.mode; // "100644"
entry.treeId; // id of the tree it was resolved from
entry.isTree(); // true for directories ("040000")
entry.asTree(); // Tree | null — resolve directory entries for further traversal
```

### `TreeWalkEntry`

```ts
interface TreeWalkEntry {
  path: string; // full path from the tree root, e.g. "src/lib/util.ts"
  mode: string;
  id: string;
  content?: Buffer; // only with { contents: true }, and not for subtrees
}
```

`walk` emits entries depth-first in lexicographic-by-path order.

## `TreeBuilder`

Incremental authoring. Edits are buffered in a content-addressed prolly tree
keyed by path, so every `upsert()`/`remove()` derives a new root over the
previous one's nodes: repeated edits keep their share of untouched nodes, and
`get()` costs `O(log n)` instead of scanning the whole edit history.

```ts
const builder = Tree.build(repo, baseTreeId?); // or new TreeBuilder(repo, baseTreeId?)

builder.upsert(path, id, mode?); // insert or overwrite; nested paths create intermediate trees
builder.remove(path, removeMode?); // "any" (default) | "leaf" | "ifLeaf"
builder.file(path, content, mode?); // write content as a blob, then upsert
builder.get(path); // TreeEntry | null — the current effective state
builder.clear(); // reset to an empty-tree base, dropping all edits

builder.write(); // Tree — prolly-backed
builder.writeProlly(); // 64-hex prolly root
```

### Writing is prolly-native

`write()` applies the recorded overlay to the base tree's flat map **in memory**
and canonicalises it directly — the git tree is never built by gix:

1. The base tree is flattened and the overlay edits are replayed on top of it. A
   removal cascades to descendants (unless a descendant was upserted
   afterwards), and the `leaf`/`ifLeaf` remove modes are honoured per git
   semantics.
2. The resulting map is canonicalised into the prolly store.
3. The returned `Tree` is prolly-backed; its `id` is the git tree oid _derived_
   from the prolly content.

`writeProlly()` performs the same work but returns only the prolly root, and
never writes a git tree.

### Validation

`write()` and `writeProlly()` accept `{ validate }` (default `true`):

| `validate` | Effect                                                               |
| ---------- | -------------------------------------------------------------------- |
| `true`     | every referenced object id must resolve in the object database       |
| `false`    | no checks; the tree is written as-is (matching git2's `TreeBuilder`) |

With validation enabled, `write()` _also_ persists the derived git tree and all
of its subtrees to the object database, so that the derived id resolves for
`findTree`, `asObject`, checkout and commits. Pass `{ validate: false }` to keep
the result entirely in the prolly store.

## Error behavior

- `findTree` throws when the object is missing or is not a tree.
- `canonicalise()` throws when the repository's prolly node store cannot be
  written.
- `write()`/`writeProlly()` throw when a referenced object id does not resolve
  (with validation enabled), when an id is not a valid hex id, or when a removal
  with `removeMode: "leaf"` targets a tree.
- `prollyTree(root)` throws when the root is not a valid prolly content id or
  the tree cannot be read back.

## See also

- [Objects API](object.md) — blobs, commits, tags and the raw object store.
