# Revision API

Resolving revision specifications (`HEAD~2`, `main`, `v1.0.0^{commit}`, `a..b`),
walking commit history, and describing commits `git describe`-style.

All spec parsing goes through the repository's revspec parser, which accepts the
same syntax as `git rev-parse`: anchors (names, hashes, prefixes), `~`/`^`
navigation, `@{n}`/`@{<date>}` reflog lookups, `@{u}`/`@{push}` sibling
branches, `^{…}` peeling, `:path` index lookups, and range/merge/exclusion
forms.

Merge-base queries (`mergeBase`, `mergeBasesMany`, `mergeBaseOctopus` and their
`*WithGraph` variants) are revision-graph traversals too, but they are
documented with the merge API since that is how they are used.

## Revspec syntax primer

| Spec    | Kind (from `revParse`) | Meaning                                                |
| ------- | ---------------------- | ------------------------------------------------------ |
| `a`     | `"single"`             | `a` and its ancestors                                  |
| `^a`    | `"exclude"`            | everything but `a` and its ancestors                   |
| `a..b`  | `"range"`              | reachable from `b` but not from `a`                    |
| `a...b` | `"merge"`              | reachable from either side, excluding their merge base |
| `a^@`   | `"include-parents"`    | all parents of `a`, but not `a` itself                 |
| `a^!`   | `"exclude-parents"`    | `a` alone (excludes its parents)                       |

Navigation modifiers compose with any anchor: `HEAD~3`, `main^2`,
`v1.0.0^{commit}`, `@^{/fix parser}` (regex search), `:src/revision.rs` (index
lookup at stage 0).

## `Repository`

| Method           | Signature                                                                | Notes                                                                                                                                                    |
| ---------------- | ------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `revParseSingle` | `(spec: string) => string`                                               | Resolve a spec to a single hex object id. Throws on invalid, ambiguous, or multi-object specs.                                                           |
| `revParseHex`    | `(spec: string) => string`                                               | Alias of `revParseSingle`.                                                                                                                               |
| `revParse`       | `(spec: string) => RevSpec`                                              | Parse a spec and report its kind plus the resolved id(s) — distinguishes ranges, merges and exclusions.                                                  |
| `revWalk`        | `(tips: string[], options?: WalkOptions) => string[]`                    | Walk all commits reachable from the tips, in walk order. Invalid tip ids throw.                                                                          |
| `describe`       | `(commit: string, options?: DescribeOptions) => DescribeOutcome \| null` | `git describe`: closest tag/branch containing `commit`. `null` when no candidate exists and `idAsFallback` is off. Throws when `commit` is not a commit. |

## Result and option objects

```ts
interface RevSpec {
  kind:
    | "single" // a
    | "exclude" // ^a
    | "range" // a..b
    | "merge" // a...b
    | "include-parents" // a^@
    | "exclude-parents"; // a^!
  id?: string; // the primary resolved id (the `to`/`ours` side for ranges/merges)
  from?: string; // the `from` side of a range, or `theirs` of a merge
  to?: string; // the `to` side of a range, or `ours` of a merge
}

interface WalkOptions {
  sort?: "breadth-first" | "time-newest" | "time-oldest"; // default: breadth-first
  reverse?: boolean; // return commits oldest-first instead
  firstParent?: boolean; // traverse only the first parent of merge commits
}

interface DescribeOptions {
  names?: "annotatedTags" | "allTags" | "allRefs"; // default: annotatedTags (git's default)
  firstParent?: boolean; // speed up traversal at the cost of accuracy
  maxCandidates?: number; // default 10, capped at 32
  idAsFallback?: boolean; // always produce a result, falling back to the abbreviated id
}

interface DescribeOutcome {
  name: string | undefined; // closest ref name; absent when only the id is available
  id: string; // the described commit id
  depth: number; // commits between the named ref and `id`
  exactMatch: boolean; // true when depth === 0
  format: string; // e.g. "v1.0.0", "v1.0.0-3-gabcdef0", or the bare abbreviated id
  commitsSeen: number; // traversal cost, useful for diagnostics
}
```

Absent optional fields on result objects are **omitted** (`undefined`) — e.g. a
`"single"` spec has no `from`, and a fallback describe has no `name`. Top-level
"not found" results are `null` (`describe` without `idAsFallback`).

## Choosing describe candidates

`names` selects which references contribute candidate names, mirroring
`git describe`:

- `"annotatedTags"` (default) — only annotated tags, like plain `git describe`.
- `"allTags"` — annotated and lightweight tags.
- `"allRefs"` — every reference, including local branch names.

When several candidates match, the one with the smallest `depth` wins. With
`maxCandidates: 0` no candidates are considered at all — a quick way to test the
fallback paths deterministically.

## Working example

```ts
import { Repository } from "@alloy-ts/repository";

const repo = Repository.open(".");

// Resolve specs to ids
repo.revParseSingle("HEAD~1"); // parent of HEAD
repo.revParseHex("v1.0.0^{commit}"); // tag peeled to its commit

// Inspect spec structure
const range = repo.revParse("HEAD~2..HEAD");
// { kind: "range", from: "<id>", to: "<id>", id: "<to>" }
const merge = repo.revParse("main...feature");
// { kind: "merge", from: "<theirs>", to: "<ours>", id: "<ours>" }

// Walk history
const head = repo.headId();
repo.revWalk([head]); // breadth-first, newest tip first
repo.revWalk([head], { sort: "time-oldest", reverse: true }); // chronological order
repo.revWalk([head], { firstParent: true }); // skip side branches

// Describe commits
repo.describe(head, { names: "allTags" });
// { name: "v1.0.0", id: "<head>", depth: 0, exactMatch: true, format: "v1.0.0", commitsSeen: 1 }
repo.describe(repo.revParseSingle("HEAD~1"), { names: "allTags" });
// { name: "v1.0.0", depth: 1, exactMatch: false, format: "v1.0.0-1-g<abbrev>", ... }
```

## Error behavior

- `revParseSingle` / `revParseHex` / `revParse` throw on invalid specs with
  git-compatible messages (unknown refs, ambiguous short hashes, malformed
  navigation, unconsumed input). They also throw when the delegate cannot
  produce a revision, e.g. a `HEAD~1` on a root commit.
- `revWalk` throws for tip ids that don't exist or don't decode. An unrecognized
  `sort` value is treated as `"breadth-first"` (lenient, like the rest of the
  library's option parsing).
- `describe` throws when `commit` does not exist or is not a commit. It returns
  `null` when no candidate name was found and `idAsFallback` is off; with
  `idAsFallback` on it always returns an outcome whose `name` is absent.
- `describe` never throws for "no tag found" — that is a `null`/fallback result,
  not an error.
