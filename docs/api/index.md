# Index API

Everything around the git index (staging area): opening it in several flavors,
fast path lookups, stat information for racy-git style freshness checks,
in-memory mutation, the index extensions, and persistence — to disk or to bytes.

An `Index` wraps `gix_index::File`: a fully decoded snapshot that can be
inspected, mutated, and written back. All paths are repository-relative with `/`
separators (never absolute, never platform-native). Entry stage numbers follow
git: `0` = unconflicted, `1` = base, `2` = ours, `3` = theirs.

## Opening an index

### `Repository`

| Method                       | Signature                                             | Notes                                                                                                                                       |
| ---------------------------- | ----------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| `index`                      | `() => Index`                                         | The shared, auto-updating worktree index. Throws when no index file exists yet.                                                             |
| `openIndex`                  | `(skipHash?: boolean) => Index`                       | A fresh, fully-decoded copy from disk. With `skipHash`, the trailing-checksum verification is skipped (≈2x faster, less corruption safety). |
| `openIndexAt`                | `(path: string, options?: IndexOpenOptions) => Index` | Open an index file at an **arbitrary path** (not necessarily this repo's index), creating an empty in-memory index if the file is missing.  |
| `openIndexFile`              | `() => Index`                                         | A fresh decode of `<git_dir>/index`; throws when missing.                                                                                   |
| `tryIndex`                   | `() => Index \| null`                                 | Like `index()`, but `null` when the index file doesn't exist yet.                                                                           |
| `indexOrEmpty`               | `() => Index`                                         | The index if present, otherwise an empty in-memory index. Never throws for a missing file.                                                  |
| `indexOrLoad`                | `() => Index`                                         | The index if present, otherwise `HEAD^{tree}` expanded into an in-memory index (or empty on an unborn `HEAD`).                              |
| `indexOrLoadFromHead`        | `() => Index`                                         | Like `indexOrLoad`, but **fails on an unborn `HEAD`** instead of falling back to empty.                                                     |
| `indexFromTree`              | `(tree: string) => Index`                             | Recursively unpack a tree object into a fresh in-memory index (expensive; no extensions).                                                   |
| `indexPath`                  | `() => string`                                        | The path at which the index file lives (`<git_dir>/index`).                                                                                 |
| `indexEntries`               | `() => IndexEntry[]`                                  | Convenience: `openIndexFile().entries()`. Throws when no index exists.                                                                      |
| `indexOrLoadFromHeadEntries` | `() => IndexEntry[]`                                  | Convenience: `indexOrLoad().entries()`. Never throws for a missing index.                                                                   |
| `editIndex`                  | `(entries: IndexEntryInput[]) => void`                | Upsert entries into the on-disk index (mode defaults to `100644`). Sorts entries and drops the tree-cache extension before writing.         |

`IndexOpenOptions`:

```ts
{
  skipHash?: boolean; // skip checksum verification on open
  threadLimit?: number; // cap parallel decoding threads
  expectedChecksum?: string; // verify against this checksum (shared-index validation)
  allocLimitBytes?: number; // cap single allocations caused by untrusted index data
}
```

A missing _shared_ index (when the file carries a `link` extension) remains an
error even for `openIndexAt`'s create-fallback.

## `Index`

### General information

| Method                | Signature               | Notes                                                                                         |
| --------------------- | ----------------------- | --------------------------------------------------------------------------------------------- |
| `len`                 | `() => number`          | Number of entries.                                                                            |
| `isEmpty`             | `() => boolean`         | `true` when there are no entries.                                                             |
| `path`                | `() => string`          | The file this index was read from / will be written to.                                       |
| `version`             | `() => number`          | On-disk format version: `2`, `3`, or `4`.                                                     |
| `entries`             | `() => IndexEntry[]`    | All entries, sorted by path then stage.                                                       |
| `timestamp`           | `() => IndexTimestamp`  | `{ secs: number, nsecs: number }` — when the state was created; the baseline for racy checks. |
| `setTimestamp`        | `(secs, nsecs) => void` | Update the freshness timestamp. **Dangerous**: a wrong value causes racy-git race conditions. |
| `isSparse`            | `() => boolean`         | `true` if the index contains at least one sparse `DIR` entry.                                 |
| `hadEndOfIndexMarker` | `() => boolean`         | Whether the `EOIE` extension was present at decode time.                                      |
| `hadOffsetTable`      | `() => boolean`         | Whether the `IEOT` extension was present at decode time.                                      |

### `IndexEntry`

```ts
{
  path: string; // repository-relative, '/'-separated
  mode: string; // "100644" | "100755" | "040000" | "120000" | "160000"
  id: string; // hex object id
  stage: number; // 0..3
  stat: IndexStat;
  flags: number; // raw gix_index::entry::Flags bits
}
```

`IndexStat` mirrors `gix_index::entry::Stat`:

```ts
{
  ctimeSecs: number;
  ctimeNsecs: number;
  mtimeSecs: number;
  mtimeNsecs: number;
  dev: number;
  ino: number;
  uid: number;
  gid: number;
  size: number;
}
```

`flags` is the raw bitfield; notable bits: `ASSUME_VALID` (0x8000), `UPDATE`
(0x10000), `REMOVE` (0x20000), `SKIP_WORKTREE` (0x40000000), `INTENT_TO_ADD`
(0x20000000), `CONFLICTED` (0x800000). The stage itself is already exposed as
`stage`.

### Stat comparison helpers

Free functions (not methods, because `IndexStat` is a plain JS object):

| Function           | Signature                                                | Notes                                                                 |
| ------------------ | -------------------------------------------------------- | --------------------------------------------------------------------- |
| `indexStatIsRacy`  | `(stat, timestamp: IndexTimestamp, options?) => boolean` | `true` when the entry must be re-checked against disk (racy-git.txt). |
| `indexStatMatches` | `(a, b, options?) => boolean`                            | Compare two stats with git's comparison knobs.                        |

`IndexStatOptions` defaults mirror git:
`{ trustCtime: true, checkStat: true, useNsec: false, useStdev: false }`.

### Lookups

All lookup paths use the repository-relative format.

| Method                 | Signature                                  | Notes                                                                                           |
| ---------------------- | ------------------------------------------ | ----------------------------------------------------------------------------------------------- |
| `entryByPath`          | `(path) => IndexEntry \| null`             | Stage 0, or stage 2 (ours) in case of conflict; `null` when absent.                             |
| `entryByPathAndStage`  | `(path, stage) => IndexEntry \| null`      | Exact stage match; throws on a stage outside `0..=3`.                                           |
| `entryByPathIcase`     | `(path, ignoreCase) => IndexEntry \| null` | Any stage, optional ASCII case-folding. Linear scan (see note below).                           |
| `pathIsDirectory`      | `(path) => boolean`                        | `true` when `path` contains indexed entries or is a submodule/sparse directory. Case-sensitive. |
| `pathIsDirectoryIcase` | `(path, ignoreCase) => boolean`            | Same, with optional ASCII case-folding.                                                         |

> **Note** the `*Icase` variants are implemented as linear scans rather than
> gix's hash-accelerated lookup (`AccelerateLookup` cannot cross the FFI).
> Results are identical; large indexes pay O(n) per call.

### Mutation

Mutations are **in-memory**; nothing touches disk until `write()`.

| Method               | Signature                       | Notes                                                              |
| -------------------- | ------------------------------- | ------------------------------------------------------------------ |
| `sortEntries`        | `() => void`                    | Restore the sorted-by-(path, stage) invariant after manual pushes. |
| `removeEntryAtIndex` | `(index) => IndexEntry \| null` | Physically remove an entry; `null` when out of bounds.             |

### Extensions

| Method               | Signature                           | Notes                                                                                      |
| -------------------- | ----------------------------------- | ------------------------------------------------------------------------------------------ |
| `treeExtension`      | `() => IndexTreeExtension \| null`  | The tree-cache (`TREE`); recursive `{ name, id, numEntries?, children }`.                  |
| `linkExtension`      | `() => IndexLinkExtension \| null`  | Split-index (`link`): `{ sharedIndexChecksum, deleteBits?, replaceBits? }`.                |
| `untrackedExtension` | `() => IndexUntrackedCache \| null` | Untracked cache (`UNTR`): identifier, exclude-file stats, per-directory untracked entries. |
| `hasResolveUndo`     | `() => boolean`                     | Presence of the resolve-undo (`REUC`) extension (its data isn't publicly readable yet).    |
| `hasFsMonitor`       | `() => boolean`                     | Presence of the fs-monitor (`FSMN`) extension (same reason).                               |

The `link` extension's shared index is dissolved automatically at open time, so
split index repositories work transparently through any open method.

### Persistence

| Method          | Signature                                   | Notes                                                                                                                                                                                                                                        |
| --------------- | ------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `write`         | `(skipHash?: boolean) => number`            | Write to `path()` under a lock file; returns the version actually written. `skipHash` zeroes the trailing checksum (≈2x faster). The tree-cache extension, if present, is written as-is — drop it after mutating entries (see caveat below). |
| `writeToBuffer` | `(skipHash?: boolean) => IndexWriteOutcome` | Serialize to bytes without touching the filesystem (bare repos, archives, transport).                                                                                                                                                        |

`IndexWriteOutcome`:

```ts
{
  version: number; // 2, 3, or 4
  checksum: string | null; // the trailing checksum; null when skipHash
  data: Buffer; // the full encoded index, including the checksum
}
```

**Tree-cache caveat** (applies to both `write` and `writeToBuffer`, inherited
from gix-index): the tree extension is serialized from its in-memory state and
is _not_ recomputed to match mutated entries. After any entry mutation, code
that writes an index should drop the stale cache — `Repository.editIndex()` does
exactly that internally.

## End-to-end example

```ts
import { indexStatIsRacy, Repository } from "./src/main.ts";

const repo = Repository.open(".");
const idx = repo.index();

// Inspect entries with full stat data
for (const e of idx.entries()) {
  console.log(`${e.mode} ${e.id.slice(0, 8)} ${e.path}`);
  if (indexStatIsRacy(e.stat, idx.timestamp())) {
    // mtime is at/after the index timestamp — re-stat the file before trusting it
  }
}

// Fast lookups
const entry = idx.entryByPath("src/main.ts");
const conflicted = idx.entryByPathAndStage("merging.txt", 2); // "ours" during a conflict
const isDir = idx.pathIsDirectory("src");

// In-memory mutation, then serialize to bytes (no disk write)
idx.removeEntryAtIndex(idx.len() - 1);
idx.sortEntries();
const out = idx.writeToBuffer();
console.log(out.version, out.checksum, out.data.length);

// Open an arbitrary index file (creating an empty one if missing)
const scratch = repo.openIndexAt("/tmp/demo-index", { skipHash: true });
scratch.setTimestamp(1_700_000_000, 0);
console.log(scratch.isEmpty(), scratch.version());

// Upsert into the repository's real index (sorted + tree-cache invalidated automatically)
const blobId = repo.writeBlob(Buffer.from("hello index\n"));
repo.editIndex([{ path: "notes.txt", id: blobId, mode: "100644" }]);
```

## Error behavior

- Opening methods throw when the index file is missing, except `tryIndex`
  (`null`), `indexOrEmpty` / `indexOrLoad` (empty fallback), and `openIndexAt`
  (creates an empty in-memory index for a missing primary index).
- `entryByPathAndStage` throws on a stage outside `0..=3`; it returns `null` for
  a valid stage with no matching entry.
- All other lookup/mutation methods return `null` or a boolean instead of
  throwing when a path simply doesn't exist.
- `write` throws if the index lock file cannot be acquired (e.g. another
  writer).
