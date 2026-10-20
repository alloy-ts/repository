import { MatchGroup, RefSpec, RefSpecOperation } from "../src/main.ts";

// Refspecs are the mapping rules remotes use during fetch and push, e.g.
// `+refs/heads/*:refs/remotes/origin/*`. Everything in this example is stateless:
// no repository is needed to parse, inspect, or match refspecs.

// ---------------------------------------------------------------------------
// 1. Parsing and inspection
// ---------------------------------------------------------------------------
console.log("=== RefSpec.parse ===");
const spec = RefSpec.parse("+refs/heads/*:refs/remotes/origin/*", RefSpecOperation.Fetch);
console.log(`spec: ${spec.spec}`);
const opName = spec.operation === RefSpecOperation.Fetch ? "fetch" : "push";
console.log(`  operation=${opName} mode=${spec.mode} force=${spec.allowNonFastForward()}`);
console.log(`  source=${spec.source} destination=${spec.destination}`);
console.log(`  remote=${spec.remote} local=${spec.local}`);

const instr = spec.instruction();
console.log(
  `  instruction: ${instr.operation}/${instr.kind} allowNonFastForward=${instr.allowNonFastForward}`,
);

// Empty fetch specs are the HEAD-only spec, like git.
const headOnly = RefSpec.parse("", RefSpecOperation.Fetch);
console.log(`parse("", fetch) -> "${headOnly.spec}" (${headOnly.instruction().kind})`);

// Push specs: a bare colon pushes all matching branches, ":dst" deletes.
const all = RefSpec.parse(":", RefSpecOperation.Push);
const del = RefSpec.parse(":refs/heads/feature", RefSpecOperation.Push);
console.log(
  `push ":" -> ${all.instruction().kind}; ":refs/heads/feature" -> ${del.instruction().kind}`,
);

// Negative specs exclude refs during matching.
const negative = RefSpec.parse("^refs/heads/private", RefSpecOperation.Fetch);
console.log(`"^refs/heads/private" -> mode=${negative.mode}, kind=${negative.instruction().kind}`);

// Invalid specs throw with git-compatible messages.
try {
  RefSpec.parse("refs/heads/*:refs/remotes/origin/main", RefSpecOperation.Fetch);
} catch (e) {
  console.log(`pattern mismatch rejected: ${(e as Error).message.split("\n")[0]}`);
}

// ---------------------------------------------------------------------------
// 2. Prefixes: what a remote would be asked to advertise
// ---------------------------------------------------------------------------
console.log("\n=== Prefixes ===");
console.log(`pattern prefix: ${spec.prefix()}`);
console.log(`exact prefix:   ${RefSpec.parse("refs/heads/main", RefSpecOperation.Fetch).prefix()}`);
console.log(
  `partial name has no unambiguous prefix: ${RefSpec.parse(
    "main",
    RefSpecOperation.Fetch,
  ).prefix()}`,
);
// But partial names expand to all Git-style ref-name candidates:
console.log(
  `expandPrefixes("main"): ${RefSpec.parse("main", RefSpecOperation.Fetch)
    .expandPrefixes()
    .join(", ")}`,
);

// ---------------------------------------------------------------------------
// 3. Matching: what would a fetch actually update?
// ---------------------------------------------------------------------------
console.log("\n=== MatchGroup ===");
const group = MatchGroup.parseSpecs(
  ["+refs/heads/*:refs/remotes/origin/*", "^refs/heads/private"],
  RefSpecOperation.Fetch,
);

// 40-hex object ids for the items we pretend the remote advertised.
const sha = (n: number) => n.toString(16).padStart(40, "0");

const mappings = group.matchLhs([
  { fullRefName: "refs/heads/main", target: sha(1) },
  { fullRefName: "refs/heads/topic", target: sha(2) },
  { fullRefName: "refs/heads/private", target: sha(3) }, // excluded by the negative spec
  { fullRefName: "refs/tags/v1.0", target: sha(4) }, // matches nothing
]);
console.log(`matchLhs -> ${mappings.length} mapping(s):`);
for (const m of mappings) {
  console.log(`  [${m.specIndex}] ${m.lhs} -> ${m.rhs} (item #${m.itemIndex})`);
}

// Reverse direction: which remote ref does a tracking branch come from?
const reverse = group.matchRhs([
  {
    fullRefName: "refs/remotes/origin/main",
    target: sha(1),
  },
]);
console.log(`matchRhs -> ${reverse[0].lhs} <- ${reverse[0].rhs}`);

// Object-id sources are emitted directly, without matching any item.
const pinned = MatchGroup.parseSpecs([`${sha(0xab)}:refs/heads/pinned`], RefSpecOperation.Fetch);
const direct = pinned.matchLhs([]);
console.log(
  `object-id source -> lhs=${direct[0].lhs.slice(0, 8)}… lhsIsObjectId=${direct[0].lhsIsObjectId}`,
);

// ---------------------------------------------------------------------------
// 4. Validation: conflicts throw, partial destinations are fixed away
// ---------------------------------------------------------------------------
console.log("\n=== Validation ===");
const clean = group.matchLhsValidated([
  {
    fullRefName: "refs/heads/main",
    target: sha(1),
  },
]);
console.log(`clean run: ${clean.mappings.length} mapping(s), ${clean.fixes.length} fix(es)`);

const conflicting = MatchGroup.parseSpecs(
  ["refs/heads/a:refs/tags/x", "refs/heads/b:refs/tags/x"],
  RefSpecOperation.Fetch,
);
try {
  conflicting.matchLhsValidated([
    { fullRefName: "refs/heads/a", target: sha(1) },
    { fullRefName: "refs/heads/b", target: sha(2) },
  ]);
} catch (e) {
  console.log(`conflict rejected: ${(e as Error).message.split("\n")[0]}`);
}

console.log("\nrefspec example complete.");
