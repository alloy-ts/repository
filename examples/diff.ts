import { Algorithm, diff, DiffHunk, Repository } from "../src/main.ts";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

// Demonstrates the line-diff algorithm bindings in src/diff.rs:
//   - diff(before, after, algorithm?) -> DiffHunk[]   (the imara-diff layer)
//   - Repository.diffTreeToTree(old, newId) -> Change[] (the gix tree layer)
// The three Algorithm variants (Histogram / Myers / MyersMinimal) compute the
// same kind of edit script but with different strategies.

// ---------------------------------------------------------------------------
// Rendering helpers
// ---------------------------------------------------------------------------

// Render hunks as a readable unified-style diff. `before`/`after` are the raw
// texts; imara splits on newline-terminated tokens, so splitting on "\n" keeps
// token index i aligned with display line i.
function toUnified(before: string, after: string, hunks: DiffHunk[]): string {
  const b = before.split("\n");
  const a = after.split("\n");
  const lines: string[] = [];
  for (const h of hunks) {
    const bCount = h.beforeEnd - h.beforeStart;
    const aCount = h.afterEnd - h.afterStart;
    lines.push(`@@ -${h.beforeStart + 1},${bCount} +${h.afterStart + 1},${aCount} @@`);
    for (let i = h.beforeStart; i < h.beforeEnd; i++) {
      lines.push(`- ${b[i] ?? ""}`);
    }
    for (let i = h.afterStart; i < h.afterEnd; i++) {
      lines.push(`+ ${a[i] ?? ""}`);
    }
  }
  return lines.length ? lines.join("\n") : "(no changes)";
}

function algoName(algo?: Algorithm): string {
  if (algo === undefined) return "default (Histogram)";
  if (algo === Algorithm.Histogram) return "Histogram";
  if (algo === Algorithm.Myers) return "Myers";
  return "MyersMinimal";
}

function demo(label: string, before: string, after: string, algo?: Algorithm) {
  const name = algoName(algo);
  const hunks = diff(before, after, algo);
  console.log(`--- ${label} [${name}] -> ${hunks.length} hunk(s) ---`);
  console.log(toUnified(before, after, hunks));
  console.log("");
}

// ---------------------------------------------------------------------------
// 1. A localized edit: every algorithm agrees
// ---------------------------------------------------------------------------
console.log("=== Localized change: all algorithms agree ===");
const beforeLocal = `fn main() {
    println!("hello");
}
`;
const afterLocal = `fn main() {
    println!("hello, world");
}
`;
demo("Histogram", beforeLocal, afterLocal, Algorithm.Histogram);
demo("Myers", beforeLocal, afterLocal, Algorithm.Myers);
demo("MyersMinimal", beforeLocal, afterLocal, Algorithm.MyersMinimal);
demo("Default (no algorithm arg)", beforeLocal, afterLocal);

// ---------------------------------------------------------------------------
// 2. Restructuring with repeated lines: strategies can diverge
// ---------------------------------------------------------------------------
// Myers is a greedy LCS search and tends to produce one large hunk that spans
// unchanged context. Histogram (patience-family) avoids matching on repeated,
// low-information lines, often splitting the change into smaller, easier-to-read
// hunks. MyersMinimal drops the early-abort heuristics for a guaranteed-minimal
// edit sequence (and can be the slowest on large inputs). The outputs below may
// differ in hunk count and boundaries even though the end result is equivalent.
console.log("=== Restructured block: algorithms may diverge ===");
const beforeMove = `A
B
C
D
E
`;
const afterMove = `D
E
A
B
C
`;
demo("Histogram", beforeMove, afterMove, Algorithm.Histogram);
demo("Myers", beforeMove, afterMove, Algorithm.Myers);
demo("MyersMinimal", beforeMove, afterMove, Algorithm.MyersMinimal);

// A repeated-line case that patience/histogram specifically handles better:
const beforeDups = `# header
line
line
line
# footer
`;
const afterDups = `# header
line
line
line
line
# footer
`;
demo("Histogram", beforeDups, afterDups, Algorithm.Histogram);
demo("Myers", beforeDups, afterDups, Algorithm.Myers);

// ---------------------------------------------------------------------------
// 3. Tree-level diff (gix): compares two trees, ignores line algorithms
// ---------------------------------------------------------------------------
console.log("=== Tree diff: Repository.diffTreeToTree ===");
const repoPath = mkdtempSync(join(tmpdir(), "diff-demo-"));
const repo = Repository.init(repoPath, false);

const v1Blob = repo.writeBlob(Buffer.from("hello\n"));
const v1Tree = repo.writeTree([
  {
    path: "greeting.txt",
    mode: "100644",
    id: v1Blob,
  },
]);

const v2Blob = repo.writeBlob(Buffer.from("hello, world\n"));
const v2Tree = repo.writeTree([
  {
    path: "greeting.txt",
    mode: "100644",
    id: v2Blob,
  },
]);

const changes = repo.diffTreeToTree(v1Tree, v2Tree);
console.log(`diffTreeToTree(v1, v2) -> ${changes.length} change(s):`);
for (const c of changes) {
  console.log(`  ${c.kind} ${c.path}  (${c.oldId?.slice(0, 8)} -> ${c.newId?.slice(0, 8)})`);
}
const selfChanges = repo.diffTreeToTree(v1Tree, v1Tree);
console.log(`diffTreeToTree(v1, v1) -> ${selfChanges.length} change(s) (empty, as expected)`);

console.log("\ndiff example complete.");
