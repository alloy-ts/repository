# Attributes API

Resolving git attributes (`.gitattributes`) and ignore/exclude rules
(`.gitignore`, `.git/info/exclude`) for a path, and checking whether a path is
ignored. This is the library's interface to git's attribute and exclude
machinery — the same logic `git
check-attr` and `git check-ignore` use.

Attribute values are modeled as a tri-state that mirrors git2's `AttrValue`:

- `set` — the attribute is turned on (`attr`, or via a `[attr]` macro expansion)
- `unset` — the attribute is turned off (`-attr`)
- `value` — the attribute carries a value (`attr=value`), exposed as a string
- `unspecified` — no rule assigned the attribute for the path (`!attr`, or
  simply no matching line)

## `Repository`

| Method                         | Signature                                                                                  | Notes                                                                                      |
| ------------------------------ | ------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------ |
| `attributes`                   | `(relativePath: string, source?: AttributeSource) => AttributeEntry[]`                     | All `.gitattributes` + ignore rules for the path — what git would resolve.                 |
| `attributesOnly`               | `(relativePath: string, source?: AttributeSource) => AttributeEntry[]`                     | Like `attributes` but ignore/exclude sources are not consulted.                            |
| `attributesWithStatistics`     | `(relativePath: string, source?: AttributeSource) => AttributeResult`                      | `attributes` plus aggregate counts of where the matches came from.                         |
| `attributesOnlyWithStatistics` | `(relativePath: string, source?: AttributeSource) => AttributeResult`                      | `attributesOnly` plus the same statistics.                                                 |
| `getAttribute`                 | `(relativePath: string, name: string, source?: AttributeSource) => AttributeEntry \| null` | Resolves a single attribute — git2's `get_attr`. `null` when the attribute is unspecified. |
| `isPathExcluded`               | `(relativePath: string) => boolean`                                                        | Whether `relativePath` is ignored by `.gitignore`/exclude files.                           |
| `attributesStack`              | `(source?: AttributeSource) => AttributeStack`                                             | Build a reusable attribute + ignore cache.                                                 |
| `attributesOnlyStack`          | `(source?: AttributeSource) => AttributeStack`                                             | Reusable attribute-only cache (no ignore information).                                     |
| `excludesStack`                | `(source?: AttributeSource) => AttributeStack`                                             | Reusable exclude-only cache, for repeated `isPathExcluded` queries.                        |

`AttributeSource` selects where `.gitattributes` data is read from. Defaults to
`WorktreeThenIdMapping` (matching the previous hardcoded behavior): consult the
worktree, falling back to id mappings. `IdMapping` reads only from id mappings
(useful for bare repos or blobs from other history); `IdMappingThenWorktree`
reads id mappings first, then the worktree (used when checking files _out_).

## `AttributeStack`

A cache detached from the repository so it can outlive a single FFI call — reuse
one instance across many path queries to avoid re-reading
`.gitattributes`/`.gitignore` files every time. The query methods run the full
`at_path` + match sequence internally.

| Method                     | Signature                                                                       | Notes                                                                     |
| -------------------------- | ------------------------------------------------------------------------------- | ------------------------------------------------------------------------- |
| `attributes`               | `(relativePath: string, mode?: number) => AttributeEntry[]`                     | Like `Repository.attributes`; `mode` is the entry kind (omit for a file). |
| `getAttribute`             | `(relativePath: string, name: string, mode?: number) => AttributeEntry \| null` | Single-attribute lookup, cached. `null` when unspecified.                 |
| `attributesWithStatistics` | `(relativePath: string, mode?: number) => AttributeResult`                      | `attributes` plus statistics, taken from the stack's live counters.       |
| `isExcluded`               | `(relativePath: string) => boolean`                                             | Like `Repository.isPathExcluded`, cached.                                 |
| `statistics`               | `() => AttributeStatistics`                                                     | Counters since the stack was created (or last `takeStatistics`).          |
| `takeStatistics`           | `() => AttributeStatistics`                                                     | Same as `statistics`, then resets the counters for the next batch.        |

`mode` is the numeric file mode (see the entry-mode constants); pass it to treat
the path as a directory or a non-regular file, otherwise it is treated as a
plain file.

## Result and option objects

```ts
type AttributeState = "set" | "unset" | "value" | "unspecified";

interface AttributeEntry {
  name: string; // "myattr", "binary", "text"
  state: AttributeState;
  value?: string; // the assigned value ("hello"); present only when state === "value"
}

interface AttributeResult {
  entries: AttributeEntry[]; // the resolved attribute assignments
  statistics: AttributeStatistics; // where the matches came from
}

interface AttributeStatistics {
  patternsBuffers: number; // pattern buffers read from the index (id mappings)
  patternFiles: number; // .gitattributes files read from disk
  triedPatternFiles: number; // .gitattributes files we attempted to find on disk
}

enum AttributeSource {
  IdMapping, // read id mappings only
  IdMappingThenWorktree, // id mappings, then worktree
  WorktreeThenIdMapping, // worktree, then id mappings (default)
}
```

Absent optional fields are **omitted** (`undefined`) — `value` is missing
whenever `state` is not `"value"`, and `getAttribute` returns `null` (not an
`AttributeEntry` with `state: "unspecified"`) when the attribute is unspecified.
Non-UTF-8 attribute values decode lossily into the JS string.

## Working example

```ts
import { AttributeSource, Repository } from "@alloy-ts/repository";

const repo = Repository.open(".");

// All attributes for a path (what git would resolve)
repo.attributes("src/lib.rs");
// [{ name: "text", state: "set" }, { name: "eol", state: "value", value: "lf" }, ...]

// The built-in `binary` attribute: set, no value
repo.attributes("img.png").find((e) => e.name === "binary");
// { name: "binary", state: "set" }

// A single attribute — git2's get_attr
repo.getAttribute("src/lib.rs", "eol"); // { name: "eol", state: "value", value: "lf" }
repo.getAttribute("src/lib.rs", "no-such-attr"); // null

// Ignore checks
repo.isPathExcluded("node_modules/foo.js"); // true
repo.isPathExcluded("src/lib.rs"); // false

// Reuse a stack across many queries (and watch the cost counters)
const stack = repo.attributesStack(AttributeSource.WorktreeThenIdMapping);
stack.attributes("a.txt");
stack.isExcluded("b.log");
stack.getAttribute("c.rs", "rustfmt");
stack.statistics(); // { patternsBuffers, patternFiles, triedPatternFiles }
stack.takeStatistics(); // same, then resets
```

## Error behavior

- `attributes` / `attributesOnly` (and the `WithStatistics` variants) throw only
  on internal failures — reading the index, the worktree, or building the
  attribute cache. A path with no matching rules is **not** an error: it yields
  an empty array (or `Statistics` with zeroed counters).
- `getAttribute` throws under the same conditions; when the attribute is simply
  unspecified for the path it returns `null`, which is a normal result, not an
  error.
- `isPathExcluded` throws only on cache-building failures; it returns a boolean
  for any path, including paths with no ignore rules (`false`).
- `mode`/`AttributeSource` are advisory; an unknown `AttributeSource` string is
  rejected as a type error before any git work runs.
