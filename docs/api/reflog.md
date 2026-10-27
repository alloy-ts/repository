# Reflog

Read and mutate a reference's reflog.

gix has no in-memory, mutable `Reflog` object (git2's model); it writes reflog
entries only through reference-transaction `RefEdit`s. This module maps git2's
`Reflog` surface onto gix as closely as possible, persisting changes
immediately.

## Reading

```ts
const repo = Repository.open(dir);

// All entries, oldest first (pass `true` for most-recent-first).
const entries: ReflogEntry[] = repo.reflog("HEAD");
const exists: boolean = repo.reflogExists("HEAD");
```

`ReflogEntry` is a flat DTO — git2's `ReflogEntry` accessors map directly:

| git2 `ReflogEntry` | `ReflogEntry` field       |
| ------------------ | ------------------------- |
| `committer()`      | `signature` (`Signature`) |
| `id_new()`         | `newOid`                  |
| `id_old()`         | `previousOid`             |
| `message()`        | `message`                 |

## Mutable handle

```ts
const reflog = repo.openReflog("refs/heads/main");
reflog.append(
  "abcd".repeat(10),
  { name: "A", email: "a@example.com", time: 0, timezoneOffset: 0 },
  "work in progress",
);

const count = reflog.len(); // number of entries
const empty = reflog.isEmpty(); // boolean
const mostRecent = reflog.get(0); // index 0 = most recent entry, or null
const all = reflog.entries(true); // most-recent-first
```

`append(newOid, committer, message?)` writes a reflog line immediately. The
`previousOid` is taken from the reference's current tip (null when the reflog is
empty); the reflog is created if it does not yet exist.

## Best-effort file operations

These bypass gix's transaction layer and operate directly on the
`<common_dir>/logs/<name>` file. They do **not** move or delete the reference
itself, and are not safe against concurrent reflog writers.

```ts
repo.deleteReflog("refs/heads/feature"); // remove the reflog file
repo.renameReflog("refs/heads/old", "refs/heads/new"); // move the reflog file
```

## git2 `Reflog` → this module

| git2                                      | here                                       | Notes                                                                                    |
| ----------------------------------------- | ------------------------------------------ | ---------------------------------------------------------------------------------------- |
| `repo.reflog(name)`                       | `repo.openReflog(name)`                    | returns a handle, not the entries                                                        |
| `Reflog::append(oid, committer, msg)`     | `Reflog.append(oid, committer, msg)`       | ✅ via `RefEdit` + `RefLog::Only`                                                        |
| `Reflog::get(i)`                          | `Reflog.get(i)`                            | index `0` = most recent                                                                  |
| `Reflog::iter()` / `len()` / `is_empty()` | `Reflog.entries()` / `len()` / `isEmpty()` | ✅                                                                                       |
| `Reflog::drop(i, rewrite)`                | —                                          | ❌ gix's `reflog_create_or_append` is append-only; removing a single line is unsupported |
| `Reflog::write()`                         | —                                          | ❌ unnecessary; gix persists per-transaction                                             |
| `git_reflog_delete(name)`                 | `repo.deleteReflog(name)`                  | ⚠️ best-effort file remove                                                               |
| `git_reflog_rename(old, new)`             | `repo.renameReflog(old, new)`              | ⚠️ best-effort file rename                                                               |
