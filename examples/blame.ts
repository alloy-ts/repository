import { Repository } from "../src/main.ts";

// Demonstrates the blame bindings in src/blame.rs:
//   - Repository.blame(file, commitId?) -> BlameOutcome        (whole-file, default options)
//   - Repository.blameWithOptions(file, commitId?, opts?)      (diff algorithm, ranges, since, renames)
// Blame attributes each region of a file to the commit that last touched it.

// We open the repository that contains this project so there is real history to blame.
const repo = Repository.open(".");
const file = "Cargo.toml";

// ---------------------------------------------------------------------------
// 1. Whole-file blame from HEAD
// ---------------------------------------------------------------------------
console.log("=== blame(): whole-file blame from HEAD ===");
const full = repo.blame(file);
console.log(`blame(${file}) -> ${full.entries.length} hunk(s)`);
for (const e of full.entries) {
  const firstLine = e.startInBlamedFile + 1; // startInBlamedFile is 0-based
  const lastLine = e.startInBlamedFile + e.len;
  console.log(
    `  lines ${firstLine}..${lastLine} by ${e.commitId.slice(0, 8)}${
      e.sourceFileName ? ` (from ${e.sourceFileName})` : ""
    }`,
  );
}
console.log(`file bytes: ${full.blob.length}`);
console.log(
  `statistics: ${full.statistics.commitsTraversed} commits, ${full.statistics.blobsDiffed} blobs diffed, ${full.statistics.treesDecoded} trees decoded`,
);
// blamePath is always null with the high-level wrapper:
console.log(`blamePath: ${full.blamePath === null ? "null (expected)" : "populated"}`);

// The raw bytes are the file content at the start commit; decode them to read it.
const text = Buffer.from(full.blob).toString("utf8");
console.log("first lines of the blamed file:");
for (const line of text.split("\n").slice(0, 3)) console.log(`  ${line}`);

// ---------------------------------------------------------------------------
// 2. Restrict to a 1-based inclusive line range
// ---------------------------------------------------------------------------
console.log("\n=== blameWithOptions(): restrict to lines 1..5 ===");
const head = repo.headCommit();
const partial = repo.blameWithOptions(file, head.id, {
  ranges: [{ start: 1, end: 5 }],
});
for (const e of partial.entries) {
  const firstLine = e.startInBlamedFile + 1;
  const lastLine = e.startInBlamedFile + e.len;
  // every returned hunk must lie within the requested 1-based inclusive range
  if (firstLine < 1 || lastLine > 5) {
    throw new Error(`hunk ${firstLine}..${lastLine} out of range`);
  }
  console.log(`  lines ${firstLine}..${lastLine} by ${e.commitId.slice(0, 8)}`);
}

// ---------------------------------------------------------------------------
// 3. Choose a diff algorithm
// ---------------------------------------------------------------------------
console.log("\n=== blameWithOptions(): diff algorithms ===");
for (const algo of ["myers", "minimal", "histogram"] as const) {
  const outcome = repo.blameWithOptions(file, null, { diffAlgorithm: algo });
  console.log(`  ${algo}: ${outcome.entries.length} hunk(s)`);
}
// An unsupported algorithm throws (patience is not available in the imara-diff backend):
try {
  repo.blameWithOptions(file, null, { diffAlgorithm: "patience" });
  console.log("  patience: unexpectedly succeeded");
} catch (err) {
  console.log(`  patience: rejected as expected (${(err as Error).message})`);
}

// ---------------------------------------------------------------------------
// 4. since cutoff + rename tracking
// ---------------------------------------------------------------------------
console.log("\n=== blameWithOptions(): since + trackRenames ===");
const recent = repo.blameWithOptions(file, null, {
  since: 0,
  trackRenames: true,
});
console.log(`  since=0, trackRenames=true -> ${recent.entries.length} hunk(s)`);

console.log("\nblame example complete.");
