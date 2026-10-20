# Diff API

Two layers of diffing live in `src/diff.rs`:

- **Line/text diff** — the `diff()` free function, backed by `gix-imara-diff`.
  Picks one of three string-editing algorithms (`Histogram`, `Myers`,
  `MyersMinimal`) and returns the contiguous regions of change between two
  texts. No repository required.
- **Tree diff** — `Repository.diffTreeToTree`, backed by `gix`. Compares two
  tree objects (or one tree against empty) and reports per-path changes.

Both return simple, serializable shapes (`DiffHunk[]` / `Change[]`) so they drop
straight into JSON output, UI diff views, or patch generation.

## Line diff: `diff()`

A free function (not a method). Tokenizes `before` and `after` into lines — each
token is a newline-terminated line, so the trailing newline counts as part of
the last line. A change of line separator is therefore itself a change.

| Function | Signature                                                             | Notes                                                                                                                                        |
| -------- | --------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| `diff`   | `(before: string, after: string, algorithm?: Algorithm) => DiffHunk[] | Compare two texts line-by-line. `algorithm` defaults to `Histogram`. Returns one `DiffHunk` per contiguous change region, in document order. |

`Algorithm` selects the edit-script strategy:

| Variant        | Value | Strategy                                                                                                         |
| -------------- | ----- | ---------------------------------------------------------------------------------------------------------------- |
| `Histogram`    | `0`   | Patience-family (histogram of common lines). Best human-readable output; the default.                            |
| `Myers`        | `1`   | Classic Myers LCS with early-abort heuristics. Near-minimal, fast in the common case.                            |
| `MyersMinimal` | `2`   | Myers LCS **without** heuristics. Guarantees a minimal edit sequence, but can be slow on large/divergent inputs. |

`DiffHunk` describes one changed region. Indices are 0-based; `End` is
exclusive, so the number of changed lines is `beforeEnd - beforeStart` (and
likewise for `after`).

```ts
interface DiffHunk {
  beforeStart: number; // inclusive first changed line in `before`
  beforeEnd: number; // exclusive end line in `before`
  afterStart: number; // inclusive first changed line in `after`
  afterEnd: number; // exclusive end line in `after`
}
```

All three algorithms compute equivalent end results for a given input; they
differ in how they group unchanged context into hunks. `Histogram` tends to keep
changes in tighter, easier-to-read hunks when there are repeated or
low-information lines, while the Myers variants are greedier.

## Tree diff: `Repository.diffTreeToTree`

| Method               | Signature                                                                                                  | Notes                                                                                                                    |
| -------------------- | ---------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| `diffTreeToTree`     | `(old?: string, newId?: string, trackRewrites?: "none" \| "renames" \| "renames-and-copies") => Change[]`   | Compare tree `newId` against `old` (or against an empty tree when `old` is omitted). One `Change` per path that differs.  |
| `diffTreeToTreeStats` | `(old?: string, newId?: string) => DiffStats`                                                              | Aggregate `git diff --stat`-style counts: files changed plus inserted/removed lines.                                     |

`trackRewrites` controls rename/copy detection: `"renames"` enables rename tracking
(git's default at 50% similarity), `"renames-and-copies"` also tracks copies, and
`"none"` disables it. An unrecognized value is treated as `"none"`. When omitted,
repository configuration decides.

`Change` is a flat, path-oriented record:

```ts
interface Change {
  path: string; // repository-relative, '/'-separated (destination path for renames/copies)
  sourceLocation: string | null; // source path for renames/copies; null otherwise
  oldId: string | null; // previous object id; null for Added
  newId: string | null; // new object id; null for Deleted
  kind: "Added" | "Modified" | "Deleted" | "Renamed" | "Copied";
  entryMode: string; // git mode after the change, octal ("100644", "100755", "040000", "120000")
  previousEntryMode: string | null; // git mode before the change (Modifications/Renames/Copies); null otherwise
}
```

`DiffStats` holds the aggregate counts:

```ts
interface DiffStats {
  filesChanged: number; // files added, removed, or modified
  linesAdded: number; // total inserted lines
  linesRemoved: number; // total removed lines
}
```

To compare the same tree against itself, pass its id for both arguments — the
result is an empty `Change[]` (and `diffTreeToTreeStats` returns all zeros). Omitting
`old` diffs an empty tree against `newId`, so every entry reports `Added`.

## Working example

```ts
import { Algorithm, diff, Repository } from "@alloy-ts/repository";

// --- line diff: choose an algorithm ---
const before = 'fn main() {\n    println!("hello");\n}\n';
const after = 'fn main() {\n    println!("hello, world");\n}\n';

// default (Histogram) and explicit variants all return DiffHunk[]
const histogram = diff(before, after); // Algorithm.Histogram
const myers = diff(before, after, Algorithm.Myers);
const minimal = diff(before, after, Algorithm.MyersMinimal);

for (const h of histogram) {
  // lines h.beforeStart..h.beforeEnd changed into h.afterStart..h.afterEnd
  console.log(
    `changed before[${h.beforeStart}..${h.beforeEnd}) -> after[${h.afterStart}..${h.afterEnd})`,
  );
}

const identical = diff("a\nb\nc\n", "a\nb\nc\n");
console.log(identical.length === 0); // true

// --- tree diff: compare two trees in a repo ---
const repo = Repository.open(".");
const head = repo.headCommit();
const changes = repo.diffTreeToTree(head.treeId, head.treeId); // same tree -> empty
console.log(changes.length === 0); // true

// build a before/after pair of trees to see a real change
const blob1 = repo.writeBlob(Buffer.from("hello\n"));
const tree1 = repo.writeTree([
  {
    path: "greeting.txt",
    mode: "100644",
    id: blob1,
  },
]);
const blob2 = repo.writeBlob(Buffer.from("hello, world\n"));
const tree2 = repo.writeTree([
  {
    path: "greeting.txt",
    mode: "100644",
    id: blob2,
  },
]);

for (const c of repo.diffTreeToTree(tree1, tree2)) {
  console.log(`${c.kind} ${c.path}  (${c.oldId?.slice(0, 8)} -> ${c.newId?.slice(0, 8)})`);
  // -> Modified greeting.txt  (xxxxxxxx -> yyyyyyyy)
}

// aggregate stats (git diff --stat style)
const stats = repo.diffTreeToTreeStats(tree1, tree2);
console.log(`${stats.filesChanged} files, +${stats.linesAdded}/-${stats.linesRemoved}`);
// -> 1 files, +1/-1

// rename tracking off — renames show up as Added/Deleted instead of Renamed
const noRenames = repo.diffTreeToTree(tree1, tree2, "none");
```

## Error behavior

- `diff()` never throws for valid inputs: it returns an empty `DiffHunk[]` for
  identical inputs and tolerates any string content (including empty strings).
  The `algorithm` argument must be a valid `Algorithm` variant — napi validates
  the enum at the boundary and throws on an unrecognized value.
- `diffTreeToTree` throws when a provided `newId` (or `old`) is not a valid,
  existing tree object id; it returns an empty `Change[]` (not an error) when
  the two trees are identical or when `old` is omitted (every entry is then
  `Added`). The `trackRewrites` argument accepts `"none"`, `"renames"`, or
  `"renames-and-copies"`; any other value is treated as `"none"`.
- `diffTreeToTreeStats` has the same id-validation behavior as `diffTreeToTree`
  and returns all-zero counts for an identical (or empty-vs-empty) comparison.
