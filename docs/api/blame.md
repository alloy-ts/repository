# Blame API

Blame lives in `src/blame.rs` and exposes `gitoxide`'s file blame through three
`Repository` methods. All return (directly or indirectly) a `BlameOutcome` — the
per-line attribution of a file to the commits that last touched each region,
plus the raw file bytes and some work statistics.

- `blame(file_path, commit_id?)` — the simple form; blames the whole file from a
  commit (defaulting to `HEAD`).
- `blameWithOptions(file_path, commit_id?, options?)` — the full form; lets you
  pick a diff algorithm, restrict the blamed line ranges, set a `since` cutoff,
  enable rename/copy tracking, and optionally resolve each commit's
  author/committer/summary.
- `blameLine(file_path, line, commit_id?, options?)` — convenience: return just
  the `BlameEntry` covering a single 1-based `line`, or `null` when the line is
  outside the file.

Line numbers are **1-based** (matching `git blame` and git2's
`final_start_line_number`); the old 0-based behavior was removed in `0.1.0`.

`file_path` is repository-relative and `/`-separated (e.g. `"src/main.ts"`),
matching how git addresses paths. `commit_id` is a 40-char hex object id; omit
it to start from `HEAD`.

## Methods

| Method             | Signature                                                                                           | Notes                                                                                                                       |
| ------------------ | --------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| `blame`            | `(filePath: string, commitId?: string) => BlameOutcome`                                             | Blame the whole file from `commitId` (or `HEAD`). Equivalent to `blameWithOptions(filePath, commitId, undefined)`.          |
| `blameWithOptions` | `(filePath: string, commitId?: string, options?: BlameOptions) => BlameOutcome`                     | Blame with control over the diff algorithm, line ranges, a `since` cutoff, rename/copy tracking, and per-commit resolution. |
| `blameLine`        | `(filePath: string, line: number, commitId?: string, options?: BlameOptions) => BlameEntry \| null` | Return only the hunk covering `line` (1-based). `null` if `line` is outside the file or `0`.                                |

## Types

```ts
interface Signature {
  name: string; // author/committer display name
  email: string; // author/committer email
  time: number; // commit time, unix seconds
  timezoneOffset: number; // author/committer timezone, minutes east of UTC
}

interface BlameEntry {
  startInBlamedFile: number; // 1-based line where this hunk begins in the blamed file
  startInSourceFile: number; // 1-based line where it begins in the source file (differs if the file moved)
  len: number; // number of lines this hunk spans
  commitId: string; // 40-char hex id of the commit that introduced the hunk
  sourceFileName?: string; // present only when the hunk came from a renamed file (else undefined)
  author?: Signature; // author of the introducing commit — only when `resolveCommits` is set
  committer?: Signature; // committer of the introducing commit — only when `resolveCommits` is set
  summary?: string; // first paragraph of the introducing commit message — only when `resolveCommits` is set
}

interface BlameStatistics {
  commitsTraversed: number; // commits walked until the blame completed
  treesDecoded: number; // trees decoded to locate the file's entry
  treesDiffed: number; // tree diffs to detect add/delete/modify
  treesDiffedWithRewrites: number; // tree diffs to detect renames/rewrites
  blobsDiffed: number; // blobs compared to learn what changed
}

interface BlamePathEntry {
  sourceFilePath: string;
  previousSourceFilePath: string | null;
  commitId: string;
  blobId: string;
  previousBlobId: string;
  parentIndex: number;
}

interface BlameOutcome {
  entries: BlameEntry[]; // the hunks, in file order, non-overlapping
  blob: Uint8Array; // raw bytes of the blamed file at the start commit
  statistics: BlameStatistics; // work accounting
  blamePath: BlamePathEntry[] | undefined; // always absent (see "Limitations")
}

interface BlameRange {
  start: number; // 1-based, inclusive first line
  end: number; // 1-based, inclusive last line
}

interface BlameOptions {
  diffAlgorithm?: "myers" | "minimal" | "histogram"; // defaults to the repo's configured algorithm
  ranges?: BlameRange[]; // 1-based inclusive ranges to blame; omit for the whole file
  since?: number; // only consider commits at/after this unix timestamp (seconds)
  trackRenames?: boolean; // track renames/rewrites at the git default 50% similarity
  trackCopies?: boolean; // also track copies (implies rename tracking)
  resolveCommits?: boolean; // attach author/committer/summary (default false)
}
```

> **Note on `undefined` vs `null`:** napi serializes absent optional _object_
> fields (`sourceFileName`, `author`, `committer`, `summary`, `blamePath`) as
> `undefined` in JavaScript, not `null`. Only `Option` values returned directly
> from a function are materialized as `null`.

`entries` cover the entire file with no gaps and in ascending
`startInBlamedFile` order, so `sum(entry.len)` equals the number of blamed
lines. With `blameLine`, the returned entry satisfies
`line >= startInBlamedFile && line < startInBlamedFile + len`. `blob` is the
file's content as bytes, which you can decode with
`Buffer.from(outcome.blob).toString("utf8")` (Node) or
`new TextDecoder().decode(...)`.

## Working example

```ts
import { Repository } from "@alloy-ts/repository";

const repo = Repository.open(".");
const file = "Cargo.toml";

// --- simple: full-file blame from HEAD ---
const full = repo.blame(file);
console.log(`blame(${file}) -> ${full.entries.length} hunk(s)`);
for (const e of full.entries) {
  // 1-based inclusive range: [start, start + len - 1]
  console.log(
    `  lines ${e.startInBlamedFile}..${e.startInBlamedFile + e.len - 1} by ${e.commitId.slice(
      0,
      8,
    )}`,
  );
}
console.log(
  `file has ${full.blob.length} bytes; traversed ${full.statistics.commitsTraversed} commits`,
);

// --- restrict to the first 5 lines (1-based, inclusive) ---
const head = repo.headCommit();
const partial = repo.blameWithOptions(file, head.id, {
  ranges: [{ start: 1, end: 5 }],
});
for (const e of partial.entries) {
  const last = e.startInBlamedFile + e.len - 1;
  if (e.startInBlamedFile < 1 || last > 5) throw new Error("out of range");
}

// --- choose a diff algorithm ---
const byMyers = repo.blameWithOptions(file, null, { diffAlgorithm: "myers" });
const byHistogram = repo.blameWithOptions(file, null, {
  diffAlgorithm: "histogram",
});
console.log(byMyers.entries.length, byHistogram.entries.length);

// --- only commits at/after a cutoff, with rename and copy tracking ---
const recent = repo.blameWithOptions(file, null, {
  since: 0,
  trackRenames: true,
  trackCopies: true,
});
console.log(`recent blame: ${recent.entries.length} hunk(s)`);

// --- resolve each commit's author/committer/summary (opt-in) ---
const detailed = repo.blameWithOptions(file, null, { resolveCommits: true });
for (const e of detailed.entries) {
  console.log(`${e.commitId.slice(0, 8)} ${e.summary} — ${e.author?.name}`);
}

// --- a single line, directly ---
const mid = Math.ceil(full.entries.reduce((a, e) => a + e.len, 0) / 2);
const hunk = repo.blameLine(file, mid, null);
console.log(hunk ? `line ${mid} is from ${hunk.commitId.slice(0, 8)}` : "no hunk");

// --- decode the blamed content ---
const text = Buffer.from(full.blob).toString("utf8");
console.log(text.split("\n").slice(0, 3));
```

## Error behavior

- `blame` / `blameWithOptions` throw when `filePath` does not exist at the start
  commit, when `commitId` (if given) is not a valid 40-char hex object id, or
  when the repository cannot be read. The underlying `gix` error is surfaced
  through the napi error channel.
- An unknown `diffAlgorithm` (anything other than `myers`, `minimal`,
  `histogram`) throws. `patience` is **not** supported by the `imara-diff`
  backend.
- `ranges` are validated as 1-based inclusive line ranges; an empty or inverted
  range (e.g. `start > end`) throws.
- `blameLine` does **not** throw for an out-of-range `line` — it returns `null`.
  It only throws for the same invalid-input cases as `blameWithOptions`.

## Limitations

- **`blamePath` is always absent (`undefined`).** The high-level
  `Repository::blame_file` wrapper forces `debug_track_path` off, so the debug
  trace of the history traversal is never populated. The `BlamePathEntry` type
  is exposed for completeness and would be filled in if that constraint is
  lifted.
- **`resolveCommits` adds latency.** Each _unique_ introducing commit is looked
  up once (cached across hunks), so the cost grows with the number of distinct
  commits, not the number of hunks. Leave it off when you only need `commitId`.
- **Worktree-untracked content is not blameable.** Blame always starts from a
  commit (`HEAD` by default); there is no `Start::Contents` variant exposed, so
  the working tree's uncommitted edits cannot be attributed.
- **No `patience` algorithm.** The backend is `imara-diff`, whose variants are
  `Myers`, `MyersMinimal`, and `Histogram`.
- **Not ported from the git2/libgit2 blame API.** `gitoxide`'s `blame_file`
  backend (0.89) does not expose `orig_commit_id` / original signatures,
  `is_boundary`, `first_parent`, `use_mailmap`, `ignore_whitespace`,
  `newest_commit`/`oldest_commit` bounds, or in-memory (`blame_buffer`) blaming.
  These are intentionally absent rather than stubbed as `null`.

## Changes in `0.1.0`

- **Breaking:** `BlameEntry.startInBlamedFile` / `startInSourceFile` are now
  **1-based** (previously 0-based).
- Added `BlameEntry.author`, `BlameEntry.committer`, `BlameEntry.summary`
  (populated only when `resolveCommits` is set).
- Added `Repository.blameLine(...)`.
- Added `trackCopies` and `resolveCommits` to `BlameOptions`.
