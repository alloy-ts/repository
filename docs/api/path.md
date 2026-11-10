# Path API

Standalone git-style path utilities: symlink resolution (`realpath`), virtual
`..` elimination (`normalise`), cwd-relative display paths, git's absolute-path
rule, and path-separator conversion.

Everything here is stateless and works without a repository — these functions
take and return plain strings. The byte-level `OsStr`↔bstr conversion helpers
from the underlying Rust crate (`gix-path`) are intentionally not exposed: JS
strings are already UTF-8.

Note the distinction from `Repository.normalisePath()`, which normalises a path
into a repository-relative Git path with slash separators. The functions here
are pure string operations with no repository involvement.

## `realpath` — symlink resolution

| Function              | Signature                                                     | Notes                                                                                                                                                           |
| --------------------- | ------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `pathRealpath`        | `(path: string, maxSymlinks?: number) => string`              | Resolves every symlink component like `realpath(3)`. Non-existing components are kept as-is. Relative paths are resolved against the current working directory. |
| `pathRealpathWithCwd` | `(path: string, cwd: string, maxSymlinks?: number) => string` | Same, but relative paths resolve against an explicit `cwd` (must be absolute on Windows, where drive-relative paths use the drive's CWD).                       |

Both throw when more than `maxSymlinks` (default 32, matching
`gix::path::realpath::MAX_SYMLINKS`) symlinks are followed, when a `..`
component escapes the filesystem root, or — for `pathRealpath` with a relative
input — when the current directory cannot be determined. An empty `path` throws
("Empty is not a valid path").

## `normalise` — virtual `..` elimination

Resolves relative components _virtually_, without accessing the filesystem and
without following symlinks — keeping the path similar to what a user provided.

| Function                  | Signature                                              | Notes                                                                                                                                                                                                      |
| ------------------------- | ------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `pathNormalise`           | `(path: string, currentDir: string) => string \| null` | Eliminates intermediate `..`: `a/./b/c/.././..` → `a`, `/a/../b/..` → `/`. Returns `null` when `..` exhausts `currentDir`.                                                                                 |
| `pathNormaliseSaturating` | `(path: string, currentDir: string) => string`         | Like `pathNormalise`, but `..` beyond the root is a no-op (`../../r` from `/c` → `/r`). Throws when `currentDir` is empty/relative and gets exhausted.                                                     |
| `pathNormaliseAndClean`   | `(path: string, currentDir: string) => string \| null` | Like `pathNormalise`, but also removes `.` components and duplicate or trailing separators (`a//b/.` → `a/b`). Returns `currentDir` when nothing remains; `null` when normalization exhausts `currentDir`. |

Details inherited from git's behavior:

- Leading `.` components and duplicate separators are left untouched by
  `pathNormalise` (`.///` stays `.///`); use `pathNormaliseAndClean` when those
  should go away.
- `currentDir` is consumed when the path pops past its own root; it should be
  absolute (typically `process.cwd()` or what `gix_fs::current_dir` returns).
- Symlinks are never resolved here — use `pathRealpath` when physical resolution
  matters.

## Display paths

| Function                   | Signature                                          | Notes                                                                                                                                                                                                                  |
| -------------------------- | -------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `pathRelativizeWithPrefix` | `(relativePath: string, prefix: string) => string` | Rebuild a worktree-relative path to be relative to `prefix` (the user's position), like git printing `../lib/main.rs` from a subdirectory. A no-op when `prefix` is empty; returns `.` when the result would be empty. |
| `pathIsAbsolute`           | `(path: string) => boolean`                        | `true` when the path is absolute by platform rules _or_ starts with `/`, matching git's cross-platform interpretation.                                                                                                 |
| `pathToUnixSeparators`     | `(path: string) => string`                         | Replaces backslashes with slashes unconditionally.                                                                                                                                                                     |
| `pathToNativeSeparators`   | `(path: string) => string`                         | No-op on Unix; slashes → backslashes on Windows.                                                                                                                                                                       |

`pathRelativizeWithPrefix` assumes both inputs are normalised (per
`pathNormalise`) and case-equal — incorrect inputs produce incorrect output,
matching the upstream contract.

## Working example

```ts
import {
  pathIsAbsolute,
  pathNormalise,
  pathNormaliseAndClean,
  pathRealpath,
  pathRelativizeWithPrefix,
} from "@alloy-ts/repository";

// Virtual normalization — no filesystem access, no symlink resolution
pathNormalise("a/./b/c/.././..", "/cwd"); // "a"
pathNormalise("/a/../b/..", "/cwd"); // "/"
pathNormalise("../../r", "/c"); // null — pops past the current dir
pathNormaliseSaturating("../../r", "/c"); // "/r"

// Cleaning duplicates and dot components
pathNormaliseAndClean("a//b/.", "/cwd"); // "a/b"

// Symlink resolution (macOS: /tmp -> private/tmp)
pathRealpath("/tmp"); // "/private/tmp" on macOS, "/tmp" elsewhere

// Git-style display path from a subdirectory
pathRelativizeWithPrefix("src/lib/main.rs", "src/lib"); // "main.rs"
pathRelativizeWithPrefix("src/lib/main.rs", ""); // "src/lib/main.rs" (no-op)

// Cross-platform checks
pathIsAbsolute("/tmp"); // true everywhere
pathIsAbsolute("C:x"); // false (not drive-absolute)
pathToUnixSeparators("a\\b\\c"); // "a/b/c"
```

## Error behavior

- `pathRealpath` / `pathRealpathWithCwd` throw on symlink cycles (exceeding
  `maxSymlinks`), on `..` escaping the root, on an empty `path`, and — for
  relative inputs — when the (current) working directory cannot be determined.
- `pathNormaliseSaturating` throws when normalization exhausts an empty or
  relative `currentDir`; `pathNormalise` and `pathNormaliseAndClean` return
  `null` in that case.
- All other functions are total: they return strings or booleans and never throw
  for any input.
