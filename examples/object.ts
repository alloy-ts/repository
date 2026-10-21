import { Repository } from "../src/main.ts";

const repoPath = process.argv[2] ?? process.cwd();

function main() {
  console.log(`Opening repository at: ${repoPath}`);
  const repo = Repository.open(repoPath);

  console.log("\n--- Repository Empty Objects & Headers ---");
  console.log(`Empty tree OID: ${repo.emptyTree()}`);
  console.log(`Empty blob OID: ${repo.emptyBlob()}`);

  const headId = repo.headId();
  const headHeader = repo.findHeader(headId);
  console.log(`HEAD header: kind=${headHeader.kind}, size=${headHeader.size} bytes`);

  console.log("\n--- Object Cache Configuration ---");
  repo.objectCacheSize(64 * 1024 * 1024);
  const cacheSize = repo.computeObjectCacheSizeForTreeDiffs();
  console.log(`Recommended cache size for tree diffs: ${(cacheSize / 1024 / 1024).toFixed(2)} MB`);

  console.log("\n--- Commit Inspection ---");
  const commit = repo.headCommit();
  console.log(`Commit ID: ${commit.id}`);
  console.log(`Short ID:  ${commit.shortId()}`);
  console.log(`Time:      ${new Date(commit.time * 1000).toISOString()}`);
  const author = commit.author();
  if (author) {
    console.log(`Author:    ${author.name} <${author.email}>`);
  }
  console.log(`Is signed: ${commit.isSigned()}`);

  const ancestors = commit.ancestorIds();
  console.log(`Ancestor commits (${ancestors.length} total): ${ancestors.slice(0, 3).map((a) => a.slice(0, 7)).join(", ")}...`);

  console.log("\n--- Tree & Blob Inspection ---");
  const tree = commit.tree();
  console.log(`Root tree OID: ${tree.id} (${tree.entries.length} entries)`);
  for (const entry of tree.entries.slice(0, 5)) {
    console.log(`  [${entry.mode}] ${entry.name} (${entry.id.slice(0, 7)})`);
    if (entry.mode === "100644" || entry.mode === "100755") {
      const blob = repo.findBlob(entry.id);
      console.log(`    -> Blob size: ${blob.size} bytes`);
    }
  }

  console.log("\n--- GitObject Downcasting & Peeling ---");
  const gitObj = repo.findObject(headId);
  console.log(`Object kind: ${gitObj.kind}`);
  const asCommit = gitObj.asCommit();
  if (asCommit) {
    console.log(`Downcasted as commit: ${asCommit.shortId()}`);
  }
  const peeledTree = gitObj.peelToTree();
  console.log(`Peeled to tree OID: ${peeledTree.id}`);
}

main();
