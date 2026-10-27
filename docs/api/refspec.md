# Refspec API

Parsing, inspecting, and matching git refspecs — the mapping rules remotes use
during fetch and push (e.g. `+refs/heads/*:refs/remotes/origin/*`).

A refspec is a rule, not a reference: it never touches the refdb. Everything
here is stateless and works without a repository. Refspecs attached to
configured remotes are available as strings via `Remote.fetchRefspecs` /
`Remote.pushRefspecs`.

## Syntax primer

```
[+|^]src[:dst]
```

- `+` forces the update (`mode: "force"`); `^` marks a negative spec that
  _excludes_ refs (`mode: "negative"`, fetch only, no destination allowed).
- For **fetch**, `src` is on the remote and `dst` is local; for **push** it is
  the reverse (`RefSpecOperation.Fetch` / `RefSpecOperation.Push` selects
  which).
- Patterns contain exactly one `*`; both sides of a two-sided spec must agree.
- An empty _fetch_ spec parses as a `HEAD`-only spec; an empty push spec is an
  error.

## `RefSpec`

### Construction

| Member          | Signature                                                | Notes                                                                             |
| --------------- | -------------------------------------------------------- | --------------------------------------------------------------------------------- |
| `RefSpec.parse` | `(spec: string, operation: RefSpecOperation) => RefSpec` | Static factory. Throws with a git-compatible validation message on invalid specs. |

### Inspection

| Member                | Signature                   | Notes                                                                                                                                                   |
| --------------------- | --------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `spec`                | `string` (getter)           | The refspec reproduced in parseable form.                                                                                                               |
| `operation`           | `RefSpecOperation` (getter) | `"push"` or `"fetch"` as an enum value.                                                                                                                 |
| `mode`                | `string` (getter)           | `"normal"`, `"force"` (starts with `+`), or `"negative"` (starts with `^`).                                                                             |
| `allowNonFastForward` | `() => boolean`             | `true` when the spec starts with `+`.                                                                                                                   |
| `source`              | `string \| null` (getter)   | The left-hand side; takes many forms, don't rely on it being a ref name. Absent for deletions.                                                          |
| `destination`         | `string \| null` (getter)   | The right-hand side; absent for source-only specs.                                                                                                      |
| `remote`              | `string \| null` (getter)   | The explicitly stored remote side (source for fetch, destination for push). One-sided push specs return `null` even though their source is used as dst. |
| `local`               | `string \| null` (getter)   | The explicitly stored local side (destination for fetch, source for push).                                                                              |

### Prefixes

| Member           | Signature              | Notes                                                                                                                                                                                                                                                      |
| ---------------- | ---------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `prefix`         | `() => string \| null` | The unambiguous ref prefix of the fetch source (or push destination): exact `refs/…` names pass through, patterns yield the fixed part before their `*`. `null` for negative and one-sided push specs, or partial names.                                   |
| `expandPrefixes` | `() => string[]`       | Like `prefix()`, but partial names expand to all Git-style ref-name candidates: `main` → `main`, `refs/main`, `refs/tags/main`, `refs/heads/main`, `refs/remotes/main`, `refs/remotes/main/HEAD`. Useful for limiting what a remote is asked to advertise. |

### Instructions

| Member        | Signature                  | Notes                                                    |
| ------------- | -------------------------- | -------------------------------------------------------- |
| `instruction` | `() => RefSpecInstruction` | Flattens the spec into a concrete operation (see below). |

```ts
interface RefSpecInstruction {
  operation: "push" | "fetch";
  kind:
    | "all_matching_branches" // push ":" — update all matching branches
    | "delete" // push ":dst" — delete the destination ref
    | "matching" // push/fetch "src:dst" (one-sided push maps src to src)
    | "exclude" // negative specs ("^src")
    | "only" // fetch "src" — fetch without updating local branches
    | "and_update"; // fetch "src:dst"
  src: string | undefined;
  dst: string | undefined;
  allowNonFastForward: boolean;
}
```

Absent optional fields on result objects are **omitted** (`undefined`), while
class getters return `null` — the same convention the rest of this library
follows.

### Name matching & mapping

Syntactic name predicates and single-side mapping — the per-refspec equivalents
of git2's `Refspec::src_matches` / `dst_matches` / `transform` / `rtransform`.
Each wraps the `MatchGroup` machinery used by `matchLhs` / `matchRhs`,
delegating to this `RefSpec` alone.

| Member               | Signature                          | Notes                                                                                                                        |
| -------------------- | ---------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `sourceMatches`      | `(name: string) => boolean`        | `true` when `name` matches the source (left) side.                                                                           |
| `destinationMatches` | `(name: string) => boolean`        | `true` when `name` matches the destination (right) side. One-sided push specs have no destination and always return `false`. |
| `transform`          | `(name: string) => string \| null` | Map `name` on the source side to its destination; `null` when `name` does not match.                                         |
| `rtransform`         | `(name: string) => string \| null` | Reverse-map `name` on the destination side to its source; `null` when `name` does not match.                                 |

Caveats:

- A **negative** refspec never produces a mapping, so `sourceMatches` /
  `destinationMatches` return `false` for names it excludes and `transform` /
  `rtransform` return `null`. Its pattern is still readable via `spec`.
- `transform` / `rtransform` are correct for **fetch** specs (they also _work_
  for push specs), matching the `matchLhs` / `matchRhs` guarantee above.

## `MatchGroup`

A group of refspecs matched in order, handling negation, deduplication, and
one-to-many pattern mappings — the machinery behind what a fetch or push would
actually update.

### Construction

| Member                  | Signature                                                       | Notes                                                                                                 |
| ----------------------- | --------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| `MatchGroup.parseSpecs` | `(specs: string[], operation: RefSpecOperation) => MatchGroup`  | Static factory; parses every spec, throwing on the first invalid one.                                 |
| `MatchGroup.fromSpecs`  | `(specs: RefSpec[], operation: RefSpecOperation) => MatchGroup` | Static factory from already-parsed specs, keeping only those whose operation matches (fetch or push). |

### Matching

Items are refs expressed as plain objects; hex ids are validated eagerly.

```ts
interface MatchItem {
  fullRefName: string; // e.g. "refs/heads/main" — always the full name
  target: string; // 40-hex id that the ref points at
  object?: string; // 40-hex peeled id when `target` is an annotated tag
}
```

| Member              | Signature                                  | Notes                                                                                                                                                                                                                                           |
| ------------------- | ------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `matchLhs`          | `(items: MatchItem[]) => RefSpecMapping[]` | Match remote refs against the _source_ side of fetch specs, mapping left→right (e.g. `refs/heads/main` → `refs/remotes/origin/main`). Object-id sources are emitted directly, without matching items. Negative specs remove mappings invisibly. |
| `matchRhs`          | `(items: MatchItem[]) => RefSpecMapping[]` | Match local tracking refs against the _destination_ side of fetch specs, reverse-mapping right→left (e.g. `refs/remotes/origin/main` → `refs/heads/main`).                                                                                      |
| `matchLhsValidated` | `(items: MatchItem[]) => ValidatedMatch`   | Like `matchLhs`, but validates destinations: conflicting destinations throw (with all issues in the message), partial destinations are dropped and reported as fixes.                                                                           |

```ts
interface RefSpecMapping {
  itemIndex?: number; // index into the items list; omitted for object-id mappings
  lhs: string; // full ref name, or a 40-hex object id
  lhsIsObjectId: boolean;
  rhs?: string; // the mapped counterpart; omitted when the spec has no destination
  specIndex: number; // which spec in the group matched
}

interface ValidatedMatch {
  mappings: RefSpecMapping[]; // what remains after validation
  fixes: MatchFix[]; // corrections applied during validation
}

interface MatchFix {
  name: string; // the partial destination ref name that was ignored
  spec: string; // the defining refspec, in parseable form
}
```

Correctness notes inherited from git: `matchLhs`/`matchRhs` are correct for
**fetch** specs (they also _work_ for push specs). Negative fetch specs exclude
refs by their source side, including after `matchRhs` reverse-mapping.

## Working example

```ts
import { MatchGroup, RefSpec, RefSpecOperation } from "@alloy-ts/repository";

// Parse and inspect a single spec
const spec = RefSpec.parse("+refs/heads/*:refs/remotes/origin/*", RefSpecOperation.Fetch);
spec.mode; // "force"
spec.source; // "refs/heads/*"
spec.prefix(); // "refs/heads/"
spec.expandPrefixes();
// ["main", "refs/main", "refs/tags/main", "refs/heads/main", ...] for partial names
spec.instruction();
// { operation: "fetch", kind: "and_update", src, dst, allowNonFastForward: true }

// Probe a name against either side, or map it through the spec
spec.sourceMatches("refs/heads/main"); // true
spec.sourceMatches("refs/tags/v1"); // false
spec.destinationMatches("refs/remotes/origin/main"); // true
spec.destinationMatches("refs/heads/main"); // false
spec.transform("refs/heads/main"); // "refs/remotes/origin/main"
spec.rtransform("refs/remotes/origin/main"); // "refs/heads/main"
spec.transform("refs/tags/v1"); // null (no match)

// Decide what a fetch of a remote's refs would update
const group = MatchGroup.parseSpecs(
  ["+refs/heads/*:refs/remotes/origin/*", "^refs/heads/private"],
  RefSpecOperation.Fetch,
);

const mappings = group.matchLhs([
  { fullRefName: "refs/heads/main", target: mainId },
  { fullRefName: "refs/heads/private", target: privateId }, // excluded
]);
// [{ lhs: "refs/heads/main", rhs: "refs/remotes/origin/main", specIndex: 0, ... }]

// Reverse direction: which remote ref does a tracking branch come from?
group.matchRhs([{ fullRefName: "refs/remotes/origin/main", target: mainId }]);
// [{ lhs: "refs/heads/main", rhs: "refs/remotes/origin/main", ... }]

// Validate before writing: conflicts throw, partial destinations are fixed away
const validated = group.matchLhsValidated(items);
validated.fixes; // destinations like "tags/x" that were dropped
```

## Error behavior

- `RefSpec.parse` and `MatchGroup.parseSpecs` throw on invalid specs with
  git-compatible messages, e.g. pattern mismatches (`a/*:b`), more than one `*`
  per side, negative specs with destinations, negative object hashes, empty push
  destinations, and empty push specs (an empty _fetch_ spec is the `HEAD`-only
  spec instead).
- `matchLhs` / `matchRhs` never throw for unmatched items — items that match
  nothing are simply absent from the result.
- `matchLhsValidated` throws when multiple sources write the same destination
  (the message lists every conflict); mappings whose destination is a partial
  ref name are removed and reported in `fixes` instead.
- Hex ids in `MatchItem` are validated up front; malformed ids throw before any
  matching.
