# Describe

`git describe`-style naming of commits, exposed on both `Repository` and
`Object`.

## Usage

```ts
const repo = Repository.open(dir);

// From a commit id:
const out = repo.describe(commitId, {
  names: "annotatedTags", // "annotatedTags" (default) | "allTags" | "allRefs"
  firstParent: false,
  maxCandidates: 10,
  idAsFallback: false,
  dirtySuffix: "-dirty", // appended only when the worktree is dirty
});

// From an object handle (convenience delegate):
const obj = repo.findObject(commitId);
const out2 = obj.describe({ idAsFallback: true });
```

`describe` returns `null` when no candidate name exists and `idAsFallback` is
off. Otherwise a `DescribeOutcome`:

```ts
interface DescribeOutcome {
  name: string | null; // closest tag/branch, or null
  id: string; // the described commit id
  depth: number; // commits between the name and the commit
  exactMatch: boolean; // commit lies directly on the named ref
  format: string; // "v1.0.0", "v1.0.0-3-gabcdef0", or the hash fallback
  commitsSeen: number; // commits traversed
}
```

## git2 `Describe` → this module

This module collapses git2's two-step `describe()` → `Describe` → `format()`
into a single call returning a fully-rendered `DescribeOutcome`, which is more
JS-friendly.

| git2                                                    | here                                                          | Notes                                                        |
| ------------------------------------------------------- | ------------------------------------------------------------- | ------------------------------------------------------------ |
| `Repository::describe(opts)` / `Object::describe(opts)` | `Repository::describe(id, opts?)` / `Object::describe(opts?)` | ✅ combined describe+format                                  |
| `Describe::format(opts)`                                | `outcome.format` (pre-rendered)                               | ✅                                                           |
| `describe.max_candidates_tags`                          | `maxCandidates`                                               | ✅                                                           |
| `describe_tags()` / `describe_all()`                    | `names` (`annotatedTags` / `allTags` / `allRefs`)             | ✅                                                           |
| `only_follow_first_parent`                              | `firstParent`                                                 | ✅                                                           |
| `show_commit_oid_as_fallback`                           | `idAsFallback`                                                | ✅                                                           |
| `DescribeFormatOptions.dirty_suffix`                    | `dirtySuffix`                                                 | ✅ via `format_with_dirty_suffix` (only appended when dirty) |
| `DescribeOptions.pattern`                               | —                                                             | ❌ gix has no glob filter over candidate refs                |
| `DescribeFormatOptions.abbreviated_size`                | —                                                             | ❌ gix renders with the repo's default abbreviation          |
| `DescribeFormatOptions.always_use_long_format`          | —                                                             | ❌ gix decides long format automatically                     |
