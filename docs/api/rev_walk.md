# Revwalk API

Incremental, configurable traversal of commit history, mirroring libgit2's
`git_revwalk`. A `Revwalk` is created with `repo.revwalk()` and then configured
by _pushing_ commits to start from (`push`, `pushHead`, `pushGlob`, `pushRange`,
`pushRef`) and _hiding_ commits to stop at (`hide`, `hideHead`, `hideGlob`,
`hideRef`). Pull commits one at a time with `next()`, or collect them all with
`toArray()`.

This is a **gix-backed** reimplementation — the libgit2 C API is not used. Where
gix and libgit2 differ, the behavior is documented under
[Divergences from libgit2](#divergences-from-libgit2).

## `Repository`

| Method    | Signature       | Notes                                                                                                 |
| --------- | --------------- | ----------------------------------------------------------------------------------------------------- |
| `revwalk` | `() => Revwalk` | Create an empty `Revwalk` bound to this repository. Configure it, then iterate with `next`/`toArray`. |

## `Revwalk`

All configuration methods return `void`. They do not start the walk — the walk
is materialised lazily on the first `next()` or `toArray()`.

### Tips and hiding

| Method      | Signature                     | Notes                                                                                                                                              |
| ----------- | ----------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| `push`      | `(spec: string) => void`      | Start from a commit. `spec` is anything `revParseSingle` accepts — an id, `HEAD`, a branch/tag name, `~`/`^`/`^{}`.                                |
| `pushHead`  | `() => void`                  | Push the repository's `HEAD`.                                                                                                                      |
| `pushGlob`  | `(glob: string) => void`      | Push the commits pointed to by references matching `glob`. An implied `refs/` prefix and `/*` suffix follow git's rules (`"v*"` → `refs/tags/v*`). |
| `pushRange` | `(range: string) => void`     | Push and hide the endpoints of a range (see [Ranges](#ranges) below).                                                                              |
| `pushRef`   | `(reference: string) => void` | Push the commit a reference resolves to (`"HEAD"`, `"main"`, `"refs/heads/main"`, …).                                                              |
| `hide`      | `(spec: string) => void`      | Mark a commit (and its ancestors) as not of interest.                                                                                              |
| `hideHead`  | `() => void`                  | Hide `HEAD`.                                                                                                                                       |
| `hideGlob`  | `(glob: string) => void`      | Hide the commits pointed to by references matching `glob`.                                                                                         |
| `hideRef`   | `(reference: string) => void` | Hide the commit a reference resolves to.                                                                                                           |

`push*` / `hide*` specs resolve through the same revspec parser as
`revParseSingle`, so they accept hashes, `HEAD~3`, `v1.0.0^{commit}`, etc. A
spec must resolve to a commit (or a commitish peeled to one); pushing a
non-commit object is an error at walk time.

### Ordering

| Method                | Signature                       | Notes                                                                                                                                        |
| --------------------- | ------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| `setSorting`          | `(sort: RevwalkSort[]) => void` | Replace the sort order. `Topological` → breadth-first (default), `Time` → commit-time (newest first), `Reverse` reverses the collected list. |
| `simplifyFirstParent` | `() => void`                    | Traverse only the first parent of each merge commit.                                                                                         |

`setSorting` **replaces** the previous ordering. `Reverse` reverses the list
after the walk (gix has no in-walk reverse), and `Topological` has no gix
equivalent so it falls back to breadth-first. To reverse without changing the
base order, pass it alongside the base flag, e.g. `["Time", "Reverse"]`. An
empty list, or `["None"]`, resets to the default (breadth-first, not reversed).

### Iteration

| Method    | Signature              | Notes                                                                                                                             |
| --------- | ---------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| `next`    | `() => string \| null` | Return the next commit id, or `null` when the walk is exhausted.                                                                  |
| `toArray` | `() => string[]`       | Return all remaining commit ids in one array. **Does not advance `next()`** — call both and you will see the same ids twice.      |
| `reset`   | `() => void`           | Clear the iteration cursor but keep the configuration (tips/hidden/sorting). The next `next()`/`toArray()` re-runs the same walk. |

The walk is materialised once into a buffer: `next()` walks that buffer and
`toArray()` returns a copy of it. Any configuration change after iterating
(`push*`, `hide*`, `setSorting`, `simplifyFirstParent`) invalidates the buffer,
so the next access rebuilds it from the updated configuration.

### Hide callback (emulation)

| Method             | Signature                                      | Notes                                                                                                                                   |
| ------------------ | ---------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `withHideCallback` | `(callback: (oid: string) => boolean) => void` | Hide every commit for which `callback` returns `true`. gix has no runtime hide callback — see [Divergences](#divergences-from-libgit2). |

Call `withHideCallback` **after** pushing your tips. It performs a one-time walk
over the currently-pushed tips, invokes `callback(oid)` for each reachable
commit, and adds every `oid` that returned `true` to the hidden set — hiding
that commit and its ancestors, exactly like libgit2's hide callback.

## `RevwalkSort`

A string enum; pass an array of these to `setSorting`:

```ts
type RevwalkSort = "None" | "Topological" | "Time" | "Reverse";
```

- `"None"` — the default (breadth-first); also implied by an empty list.
- `"Topological"` — falls back to breadth-first (gix has no topological order).
- `"Time"` — commit-time order, newest first.
- `"Reverse"` — reverse the collected list (computed after the walk).

## Ranges

`pushRange(range)` mirrors libgit2's `git_revwalk_push_range`, which pushes the
right-hand commit and hides the left-hand one:

- `"<from>..<to>"` — walk commits reachable from `to` but not from `from`. Both
  endpoints are pushed as tips; `from` (and its ancestors) is hidden.
- `"<from>...<to>"` — the symmetric range: walk commits reachable from _either_
  side but not from _both_. Both endpoints are pushed; their **merge base** (and
  its ancestors) is hidden.

## Working example

```ts
import { Repository } from "@alloy-ts/repository";

const repo = Repository.open(".");
const walk = repo.revwalk();

// Push a head and walk it commit-by-commit
walk.pushHead();
let id: string | null;
while ((id = walk.next()) !== null) {
  console.log(id); // HEAD, then its parents, breadth-first
}

// Re-run the same walk as an array
walk.reset();
const all = walk.toArray(); // same ids as the loop above

// Hide a tag and everything reachable from it
const walk2 = repo.revwalk();
walk2.pushHead();
walk2.hideRef("v1.0.0");
walk2.toArray(); // commits after v1.0.0 only

// Range walk: commits in `main` but not in `v1.0.0`
const range = repo.revwalk();
range.pushRange("v1.0.0..main");
range.toArray();

// Symmetric range: commits reachable from either side, minus the merge base
const sym = repo.revwalk();
sym.pushRange("v1.0.0...main");
sym.toArray();

// Reference globs (refs/ and /* are implied)
const tags = repo.revwalk();
tags.pushGlob("refs/tags/v*");
tags.pushGlob("v*"); // identical: -> refs/tags/v*
tags.toArray();

// Sort by commit time, reversed
const chronological = repo.revwalk();
chronological.pushHead();
chronological.setSorting(["Time", "Reverse"]);
chronological.toArray(); // oldest first

// Dynamic hiding via callback
const head = repo.revParseSingle("HEAD");
const custom = repo.revwalk();
custom.pushHead();
custom.withHideCallback((oid) => oid === head); // hide just HEAD -> empty walk
custom.toArray(); // []
```

## Error behavior

- `push` / `hide` / `pushRef` / `hideRef` throw when the spec does not resolve
  to a commit: unknown ref, ambiguous short hash, malformed navigation, a
  delegate that cannot produce a revision (e.g. `HEAD~1` on a root commit), or
  the object exists but is not a commit.
- `pushGlob` / `hideGlob` throw only when the reference store cannot be read; a
  glob matching nothing simply pushes/hides nothing (no error).
- `pushRange` throws for a malformed range (only `"<from>..<to>"` or
  `"<from>...<to>"` are accepted) and when either side is empty. A symmetric
  range with no merge base between its two sides also throws.
- `setSorting` accepts only `RevwalkSort` values; an unknown string throws.
- `withHideCallback` rarely throws on its own (any gix walk error surfaces
  through napi), but an exception thrown _inside_ `callback` is reported through
  the napi error channel.
- `next` / `toArray` throw when the walk cannot be built — e.g. no tips were
  pushed, or a pushed tip does not exist.

## Divergences from libgit2

- **No topological sort.** libgit2's `GIT_SORT_TOPOLOGICAL` has no gix
  equivalent; `RevwalkSort.Topological` falls back to breadth-first (the
  default). There is no guarantee a parent is emitted before all of its
  children.
- **No in-walk reverse.** `RevwalkSort.Reverse` reverses the _collected_ list
  after the walk completes, rather than walking backwards.
- **`withHideCallback` is emulated.** gix exposes no runtime hide callback, so
  the callback is consulted once, up front, over the commits currently reachable
  from the pushed tips; each `true` return adds that commit's id to the hidden
  set. The callback therefore cannot depend on walk order or on decisions for
  earlier commits, and hiding always means "the commit and its ancestors" —
  which matches libgit2's hide-callback semantics but precludes dynamic
  strategies.
- **`toArray()` is non-consuming.** It returns a copy of the remaining ids
  without advancing `next()`; in libgit2 the walk is consumed as you read it, so
  calling both would not replay the same commits.
- **`reset()` keeps the configuration.** libgit2's `git_revwalk_reset` also
  keeps tips, but here the next iteration re-runs the _same_ configured walk, so
  commits already returned will be returned again (libgit2 would skip them).
- **The walk is materialised.** All reachable commit ids are collected into an
  in-memory buffer at the first `next()`/`toArray()`; memory use is proportional
  to the number of commits in the walk.
