import { Repository } from "../dist/index.js";

console.log("=== Repository API Examples ===");

// 1. Open current directory repository
const repo = Repository.open(process.cwd());
console.log(`Git Dir: ${repo.gitDir()}`);
console.log(`Work Dir: ${repo.workDir()}`);
console.log(`Is Bare: ${repo.isBare()}`);
console.log(`HEAD OID: ${repo.headId()}`);

// 2. Reading Git Log
console.log("\n--- Git Log (recent 5 commits) ---");
const logEntries = repo.log("HEAD", 5, false);
for (const entry of logEntries) {
  console.log(`Commit: ${entry.id.substring(0, 7)} - ${entry.subject}`);
  console.log(`  Author: ${entry.authorName} <${entry.authorEmail}>`);
}

// 3. Revisions & Revision Walking
console.log("\n--- Revisions ---");
const headOid = repo.revParseSingle("HEAD");
console.log(`HEAD OID (revParseSingle): ${headOid}`);

const walkedOids = repo.revWalk([headOid]);
console.log(`Walked ${walkedOids.length} commits starting from HEAD`);

// 4. References
console.log("\n--- References ---");
const refs = repo.references();
console.log(`Total References: ${refs.length}`);
for (const ref of refs.slice(0, 5)) {
  console.log(`  ${ref.name} -> ${ref.target ?? ref.peeled}`);
}

// Note on Git Reflog:
// High-level reflog / history iteration is accessed via repo.log("HEAD") or revWalk.
