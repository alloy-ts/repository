import { Repository } from "../dist/index.js";

async function main() {
  console.log("Opening repository...");
  const repo = Repository.open(process.cwd());

  const headId = repo.headId();
  console.log(`HEAD ID: ${headId}`);

  // 1. Commit inspection
  const commit = repo.headCommit();
  console.log("\n--- Commit Details ---");
  console.log(`ID: ${commit.id}`);
  console.log(`Tree ID: ${commit.treeId}`);
  console.log(`Message: ${commit.message.trim()}`);
  console.log(`Parents: ${commit.parentIds.join(", ") || "(none)"}`);
  if (commit.author) {
    console.log(`Author: ${commit.author.name} <${commit.author.email}>`);
  }

  // 2. Tree inspection
  const tree = repo.findTree(commit.treeId);
  console.log("\n--- Tree Entries ---");
  for (const entry of tree.entries.slice(0, 5)) {
    console.log(`[${entry.mode}] ${entry.id.slice(0, 7)}`);
  }

  // 3. Blob inspection
  const readmeEntry = tree.entry("ReadMe.md") || tree.entry("package.json");
  if (readmeEntry) {
    const blob = repo.findBlob(readmeEntry.id);
    console.log("\n--- Blob Details ---");
    console.log(`ID: ${blob.id}`);
    console.log(`Size: ${blob.size} bytes`);
    console.log(`Preview:\n${blob.content().toString("utf-8").slice(0, 80)}...`);
  }

  // 4. Generic GitObject & downcasting
  const obj = repo.findObject(headId);
  console.log("\n--- GitObject ---");
  console.log(`Kind: ${obj.kind}`);
  const asCommit = obj.asCommit();
  if (asCommit) {
    console.log(`Downcasted to Commit: ${asCommit.id}`);
  }

  // 5. Revision spec & pathspec matching
  console.log("\n--- Revisions & Pathspec ---");
  const revSpec = repo.revParseSpec("HEAD");
  console.log(`RevSpec mode: ${revSpec.mode}, singleId: ${revSpec.singleId}`);

  const pathspec = repo.matchPathspec(["*.md", "*.json"], "ReadMe.md");
  console.log(`Match '*.md' on 'ReadMe.md': ${pathspec.isMatch}`);
}

main().catch((err) => {
  console.error("Error running examples/object.ts:", err);
  process.exit(1);
});
