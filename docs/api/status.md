# Status

Design for the status NAPI bindings in `src/status.rs`, using `git2`'s
`status.rs` as the feature checklist and `gix` 0.89's status pipeline as the
engine. The goal is a **JS-friendly, porcelain-compatible** API — not a raw port
of either.

## Current bindings (v0)

| Method / Type     | Shape                              | Problems                                                                                          |
| ----------------- | ---------------------------------- | ------------------------------------------------------------------------------------------------- |
| `status()`        | `Promise<StatusEntry[]>`           | `status` is a Rust `Debug` string (`format!("{item:?}")`) — unstable, undocumented, useless in JS |
| `indexWorktree()` | `Promise<StatusEntry[]>`           | Same; index↔worktree only                                                                         |
| `StatusEntry`     | `{ path: string; status: string }` | No HEAD→index side, no untracked/ignored options, no renames, no pathspec                         |

Note: the current `status()` **never looks at HEAD** — it only runs the
index↔worktree pass. There is no staged/unstaged distinction at all today.

## Feature inventory: git2 → gix 0.89 → ours

| git2 feature                                 | gix 0.89 equivalent                                                  | Plan                                                      |
| -------------------------------------------- | -------------------------------------------------------------------- | --------------------------------------------------------- |
| `StatusShow` (Index/Workdir/IndexAndWorkdir) | full iter (tree+index+worktree) vs `into_index_worktree_iter`        | `show` option                                             |
| `StatusOptions::pathspec`                    | `into_iter(patterns)` / `index_worktree_status(patterns)`            | ✅ `pathspec`                                             |
| `include_untracked`                          | dirwalk enabled by default; `UntrackedFiles::{None,Collapsed,Files}` | ✅ `untracked`                                            |
| `include_ignored` / `recurse_ignored_dirs`   | `dirwalk_options().emit_ignored(EmissionMode)`                       | ✅ `ignored`                                              |
| `recurse_untracked_dirs`                     | `UntrackedFiles::Files`                                              | ✅ folded into `untracked: "files"`                       |
| `renames_head_to_index`                      | `tree_index_track_renames(TrackRenames)`                             | ✅ `headToIndexRenames`                                   |
| `renames_index_to_workdir`                   | `index_worktree_rewrites(Rewrites)`                                  | ✅ `indexToWorkdirRenames`                                |
| `renames_from_rewrites`                      | `gix_diff::Rewrites` (copy tracking)                                 | ✅ inside rewrites object                                 |
| `rename_threshold`                           | `Rewrites { percentage, limit, copy_percentage }`                    | ✅ inside rewrites object                                 |
| `exclude_submodules`                         | `Submodule::{AsConfigured, Given{ignore}}`                           | ✅ `excludeSubmodules`                                    |
| `update_index` (stat cache writeback)        | `Outcome::write_changes()`                                           | ✅ `updateIndex`                                          |
| `disable_pathspec_match`                     | literal pathspec (`:(literal)` magic)                                | ✅ `disablePathspecMatch`                                 |
| `sort_case_sensitively/insensitively`        | `Sorting::ByPathCaseSensitive` only                                  | ❌ case-sensitive sort only; case-insensitive unsupported |
| `include_unmodified`                         | no direct equivalent (`EntryStatus::NeedsUpdate` is internal)        | ❌                                                        |
| `include_unreadable(_as_untracked)`          | not exposed by gix status                                            | ❌                                                        |
| `no_refresh`                                 | not applicable (gix reads the index fresh)                           | ❌                                                        |
| `status_file()`                              | none — implement as pathspec-limited status                          | ✅ `statusFile()`                                         |
| `status_should_ignore()`                     | `repo.excludes()` + attribute stack `is_excluded`                    | ✅ `shouldIgnore()`                                       |
| `is_empty/len/iter` on `Statuses`            | plain array in JS                                                    | ✅ n/a (JS arrays)                                        |
| — (git2 has none)                            | `Repository::is_dirty()` (untracked does **not** count, like git)    | ✅ `isDirty()`                                            |

## JS API design

### Principles

1. **Flat objects, no classes** — consistent with the project's flattened API
   convention (Alert/Field).
2. **String literals, not numeric enums** — `"modified"` / `"deleted"` match
   `git status --porcelain` vocabulary and isomorphic-git/simple-git ergonomics.
3. **One combined entry per path** — staged (`index`) and unstaged (`worktree`)
   sides as separate fields, plus the classic two-letter porcelain code. This
   mirrors what libgit2 gives git2, computed by merging gix's two change streams
   (HEAD→index and index→worktree) by path.

### Types

```ts
/** HEAD→index ("staged") or index→worktree ("unstaged") change class. */
type StatusSummary =
  | "added" // A  (new in index; untracked files report this on the worktree side as "untracked")
  | "modified" // M
  | "deleted" // D
  | "renamed" // R
  | "copied" // C
  | "typechange" // T  (symlink ↔ file etc.)
  | "untracked" // ?? (worktree side only)
  | "ignored" // !! (only with ignored !== "none")
  | "intent-to-add" // " A"
  | "conflicted"; // X=Y=unmerged letters, see porcelain table

interface StatusEntry {
  /** Current location of the entry (rename destination, if renamed). */
  path: string;
  /** Rename/copy source path, or `null`. */
  oldPath: string | null;
  /** HEAD→index change, or `null` if the index matches HEAD. */
  index: StatusSummary | null;
  /** Index→worktree change, or `null` if the worktree matches the index. */
  worktree: StatusSummary | null;
  /** Two-letter `git status --porcelain` code, e.g. `" M"`, `"M "`, `"AM"`, `"R "`, `"??"`. */
  porcelain: string;
  /** `true` when the entry is a collapsed untracked/ignored *directory*. */
  directory: boolean;
}

interface RewritesOptions {
  /** Similarity threshold in percent (git `-M`), default 50. */
  percentage?: number;
  /** Cap on the number of rename candidates, default 20000 (git's default). */
  limit?: number;
  /** Also track copies. */
  copies?: boolean;
}

interface StatusOptions {
  /** `"all"` (default): HEAD↔index↔worktree. `"index"`: HEAD→index only.
   *  `"worktree"`: index→worktree only. */
  show?: "all" | "index" | "worktree";
  /** Untracked file listing mode. Default: `status.showUntrackedFiles` config,
   *  i.e. `"collapsed"` when unset. `"none"` disables the directory walk. */
  untracked?: "collapsed" | "files" | "none";
  /** Ignored file listing. Default `"none"` (never listed). */
  ignored?: "none" | "matching" | "collapsed";
  /** Restrict the report to paths matching these patterns (fnmatch-style). */
  pathspec?: string[];
  /** Treat `pathspec` entries as literal paths, not patterns. */
  disablePathspecMatch?: boolean;
  /** Skip all submodule status computation. */
  excludeSubmodules?: boolean;
  /** Diff the index against this tree-ish instead of `HEAD^{tree}`. */
  head?: string;
  /** Rename/copy detection between HEAD and the index.
   *  Default `"as-configured"` (honours `status.renames` / `diff.renames`). */
  headToIndexRenames?: "none" | "as-configured" | RewritesOptions;
  /** Rename detection between index and worktree (no git config equivalent;
   *  default off, like git). */
  indexToWorkdirRenames?: boolean | RewritesOptions;
  /** Write refreshed stat info back to the index for files that are
   *  unchanged but had stale stat data (git's soft index reload). */
  updateIndex?: boolean;
}
```

### Methods

All async methods run on the libuv thread pool (`AsyncTask`).

| Method          | Signature                                             | Notes                                                                                                                                                                          |
| --------------- | ----------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `status`        | `(options?: StatusOptions) => Promise<StatusEntry[]>` | Full status. One entry per changed path, merged index + worktree sides, sorted by path when renames are enabled (gix guarantees deterministic order then); otherwise unsorted. |
| `indexWorktree` | `(options?: StatusOptions) => Promise<StatusEntry[]>` | Index↔worktree only — never touches HEAD, cheaper than `status()`. `index` is always `null`.                                                                                   |
| `isDirty`       | `() => Promise<boolean>`                              | Any staged/unstaged change (submodules included, early-exit). **Untracked files do not count**, matching `git status` semantics.                                               |
| `statusFile`    | `(path: string) => Promise<StatusEntry \| null>`      | Single-path status via pathspec-limited status run. `null` if the path is clean/unknown.                                                                                       |
| `shouldIgnore`  | `(path: string) => boolean`                           | Synchronous exclude check (`.gitignore` stack).                                                                                                                                |

### Porcelain code mapping

`porcelain = X + Y` where `X` encodes `index` and `Y` encodes `worktree`.

| JS value          | X/Y letter                         | Notes                                |
| ----------------- | ---------------------------------- | ------------------------------------ |
| `"added"`         | `A`                                |                                      |
| `"modified"`      | `M`                                |                                      |
| `"deleted"`       | `D`                                |                                      |
| `"renamed"`       | `R`                                |                                      |
| `"copied"`        | `C`                                |                                      |
| `"typechange"`    | `T`                                | git calls this `T` in short format   |
| `"untracked"`     | `?`                                | rendered as the whole code `"??"`    |
| `"ignored"`       | `!`                                | rendered as the whole code `"!!"`    |
| `"intent-to-add"` | `A` on worktree side, index `null` | rendered `" A"`                      |
| `"conflicted"`    | —                                  | replaced by the pair code, see below |

Conflict pair codes (from gix's `Conflict` analysis):

| gix `Conflict`  | porcelain | meaning         |
| --------------- | --------- | --------------- |
| `BothDeleted`   | `"DD"`    | both deleted    |
| `AddedByUs`     | `"AU"`    | added by us     |
| `DeletedByThem` | `"UD"`    | deleted by them |
| `AddedByThem`   | `"UA"`    | added by them   |
| `DeletedByUs`   | `"DU"`    | deleted by us   |
| `BothAdded`     | `"AA"`    | both added      |
| `BothModified`  | `"UU"`    | both modified   |

Examples: staged modification → `"M "`; unstaged modification → `" M"`; staged
add then further edit → `"AM"`; staged rename → `"R "`; untracked file → `"??"`
(with `index: null, worktree: "untracked"`).

### Examples

```ts
const repo = new Repository();

// git status --porcelain equivalent
const entries = await repo.status({ untracked: "files" });
for (const e of entries) console.log(e.porcelain, e.path, e.oldPath ?? "");

// staged-only view (git diff --cached --name-status equivalent)
const staged = await repo.status({
  show: "index",
  headToIndexRenames: { percentage: 40 },
});

// cheap dirty check for UI banners
if (await repo.isDirty()) warn();

// single file
const e = await repo.statusFile("src/main.rs"); // { porcelain: " M", ... } | null
if (repo.shouldIgnore("build/output.log")) skip();
```

## Rust implementation plan (`src/status.rs`)

New/changed NAPI items:

- `StatusEntry` — extended as above (path, old_path, index, worktree, porcelain,
  directory). All `String` fields hold the literal summary values.
- `StatusOptions` — `#[napi(object)]` mirror of the TS interface; unknown string
  values reject with `err_str`.
- `StatusTask` — carries `StatusOptions`; switches between
  `Platform::into_iter(patterns)` (combined) and
  `Platform::into_index_worktree_iter(patterns)` (worktree-only), mapping each
  `Item` into one or two `StatusEntry` halves and merging by path in a
  `BTreeMap<BString, Half>` (deterministic, path-sorted output).
- `StatusFileTask` — same pipeline, pathspec limited to the given path.
- `IsDirtyTask` — calls `gix`'s `Repository::is_dirty()`.
- `shouldIgnore` — sync method using `repo.excludes(None)` + `is_excluded`.

Mapping details:

- **TreeIndex items** (`gix_diff::index::Change`): `Addition` → index `"added"`;
  `Deletion` → index `"deleted"`; `Modification` → index `"modified"` (mode-only
  change still `"modified"`, or `"typechange"` when the entry mode kind
  changed); `Rewrite` → index `"renamed"`/`"copied"` with
  `oldPath = source_location`.
- **IndexWorktree `Modification`** (`EntryStatus`): `Change::Removed` → worktree
  `"deleted"`; `Change::Type` → `"typechange"`; `Change::Modification` →
  `"modified"`; `SubmoduleModification` → `"modified"`; `Conflict{summary,..}` →
  conflict pair code; `IntentToAdd` → worktree `"intent-to-add"`; `NeedsUpdate`
  → skipped (internal stat refresh), unless `updateIndex` consumes it via
  `into_outcome().write_changes()`.
- **IndexWorktree `DirectoryContents`**: `gix_dir::Status::Untracked` → worktree
  `"untracked"`; `Ignored` → `"ignored"`; `directory = kind ==
  Directory`
  (collapsed emission). Non-untracked/ignored entries are skipped.
- **IndexWorktree `Rewrite`**: worktree `"renamed"`/`"copied"`, `oldPath` from
  `RewriteSource::rela_path()`.
- **Merging**: an entry present on both sides (e.g. staged `A`, then edited)
  merges into one `StatusEntry` (`"AM"`); renames merge on the destination path;
  the index half's `oldPath` wins for `oldPath`.

## Known limitations

- No `include_unmodified`, no unreadable-file reporting, no case-insensitive
  sort (gix only supports case-sensitive deterministic sorting).
- `show: "index"` still scans the worktree unless `untracked: "none"` is set
  (the full iterator is the only combined entry point); documented, acceptable.
- `status()` return shape is a **breaking change** (the Rust-`Debug` string goes
  away). The runtime is pre-1.0; tests that assert on the old shape need
  updating.
- `statusFile` costs a full pathspec-limited status run; fine for single probes,
  not for bulk use (use `status()` instead).

## Open questions

1. Breaking change to `status()` accepted? (Recommended: yes — the old `Debug`
   string has no consumers worth keeping.)
2. Default `show`: `"all"` (porcelain parity) vs `"worktree"` (cheapest)?
   Recommended `"all"`, with `indexWorktree()` kept as the cheap path.
3. v1 scope: include `statusFile`/`shouldIgnore`/`isDirty` now, or defer?
   Recommended: include all three — each is small and they complete the git2
   checklist.
