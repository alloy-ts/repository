import { Repository } from "../src/main.ts";

const repoPath = process.argv[2] ?? process.cwd();

function main() {
  console.log(`Opening repository at: ${repoPath}`);
  const repo = Repository.open(repoPath);

  const headId = repo.headId();
  console.log(`HEAD commit ID: ${headId}`);

  // 1. Commit object
  const commit = repo.findCommit(headId);
  console.log("\n--- Commit ---");
  console.log(`ID: ${commit.id}`);
  console.log(`Tree ID: ${commit.treeId}`);
  console.log(`Parent IDs: ${commit.parentIds.join(", ") || "(none)"}`);
  console.log(`Message:\n${commit.message}`);

  // 2. Tree object
  const tree = repo.findTree(commit.treeId);
  console.log("\n--- Tree ---");
  console.log(`ID: ${tree.id}`);
  console.log(`Entries count: ${tree.entries.length}`);
  for (const entry of tree.entries.slice(0, 5)) {
    console.log(`  [${entry.mode}] ${entry.id}`);
  }

  // 3. Blob object
  const readmeEntry = tree.entry("ReadMe.md") ?? tree.entry("Cargo.toml");
  if (readmeEntry) {
    const blob = repo.findBlob(readmeEntry.id);
    const content = blob.content().toString("utf-8");
    console.log("\n--- Blob ---");
    console.log(`File: ${readmeEntry.id}`);
    console.log(`Content length: ${content.length} bytes`);
    console.log(`First line: ${content.split("\n")[0]}`);
  }

  // 4. GitObject (kinded wrapper & downcasting)
  console.log("\n--- GitObject ---");
  const obj = repo.findObject(headId);
  console.log(`Kind: ${obj.kind}`);
  const asCommit = obj.asCommit();
  if (asCommit) {
    console.log(`Successfully downcasted to Commit (${asCommit.id})`);
  }

  // 5. Annotated Tag (if any present)
  for (const ref of repo.references()) {
    if (ref.name.startsWith("refs/tags/")) {
      try {
        const tag = repo.findTag(ref.target);
        console.log("\n--- Tag ---");
        console.log(`Name: ${tag.name}`);
        console.log(`Message: ${tag.message.trim()}`);
        break;
      } catch {
        // May be a lightweight tag
      }
    }
  }
}

main();
