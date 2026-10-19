import { Repository } from "../dist/index.js";

async function main() {
  console.log("=== Repository Usage Examples ===\n");

  // 1. Open or discover a repository
  const repo = Repository.open(".");
  console.log("Working Directory:", repo.workDir());
  console.log("Git Directory:", repo.gitDir());
  console.log("Is Bare:", repo.isBare());

  // Wave 1: HEAD Management
  console.log("\n--- HEAD Management ---");
  console.log("HEAD Name:", repo.headName());
  console.log("HEAD Tree OID:", repo.headTreeId());
  const headRef = repo.headRef();
  console.log("HEAD Reference:", headRef ? headRef.name : "Detached/Unborn");

  // Wave 1: Config Entries
  console.log("\n--- Configuration Entries ---");
  const configEntries = repo.configEntries();
  for (const entry of configEntries.slice(0, 5)) {
    console.log(`Config [${entry.key}] = ${entry.value}`);
  }

  // Wave 2: Index (Staging Area)
  console.log("\n--- Index Entries ---");
  const indexEntries = repo.index();
  console.log(`Index contains ${indexEntries.length} entries.`);
  for (const entry of indexEntries.slice(0, 5)) {
    console.log(`Index [${entry.stage}] ${entry.path} (${entry.mode}) -> ${entry.id.slice(0, 8)}`);
  }

  // 2. Read git logs using repo.log(rev?, limit?, firstParent?)
  console.log("\n--- Git Log (repo.log) ---");
  const logEntries = repo.log("HEAD", 5, false);
  for (const entry of logEntries) {
    const date = new Date(entry.authorTime * 1000).toISOString().slice(0, 10);
    console.log(`${entry.id.slice(0, 8)} ${date} [${entry.authorName}] ${entry.subject}`);
  }

  // 3. Read git reflog using repo.reflog(name?)
  console.log("\n--- Git Reflog (repo.reflog) ---");
  const reflogEntries = repo.reflog("HEAD");
  for (const entry of reflogEntries.slice(0, 5)) {
    console.log(
      `${entry.previousOid.slice(0, 8)} -> ${entry.newOid.slice(0, 8)} [${entry.committer.name}]: ${entry.message}`
    );
  }

  // 4. Resolve specific file revision (e.g. ReadMe.md from main branch or HEAD)
  console.log("\n--- Resolve Revision & Read File Content ---");
  const commitOid = repo.revParseSingle("HEAD");
  console.log("Resolved HEAD SHA:", commitOid);

  const commit = repo.findCommit(commitOid);
  const tree = commit.tree();
  const readmeEntry = tree.entry("ReadMe.md");

  if (readmeEntry) {
    const blob = repo.findBlob(readmeEntry.id);
    const text = blob.content().toString("utf-8");
    console.log("ReadMe.md First Line:", text.split("\n")[0]);
  }

  // 5. Query references
  console.log("\n--- References ---");
  const refs = repo.references();
  for (const ref of refs) {
    console.log(`Ref: ${ref.name} -> ${ref.target ?? ref.peeled}`);
  }
}

main().catch(console.error);
