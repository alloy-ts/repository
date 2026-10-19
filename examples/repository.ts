import { Repository } from "../src/main.ts";

async function main() {
  // 1. Open repository
  const repo = Repository.open(".");
  console.log("Git dir:", repo.gitDir());
  console.log("HEAD id:", repo.headId());
  console.log("HEAD tree id:", repo.headTreeId());

  // 2. Read git log
  const logEntries = repo.log("HEAD", 5, false);
  console.log("\nRecent Commits:");
  for (const entry of logEntries) {
    console.log(` - ${entry.id.slice(0, 8)} ${entry.subject} (${entry.authorName})`);
  }

  // 3. Inspect config
  const configEntries = repo.configEntries();
  console.log(`\nConfig entries count: ${configEntries.length}`);

  // 4. Index and Status
  const indexEntries = repo.indexOrLoadFromHeadEntries();
  console.log(`Index entries count: ${indexEntries.length}`);

  const statusEntries = await repo.status();
  console.log(`Status entries count: ${statusEntries.length}`);

  // 5. Attributes and Submodules
  const attrs = repo.attributes("ReadMe.md");
  console.log(`Attributes for ReadMe.md count: ${attrs.length}`);

  const submodules = repo.submodules();
  console.log(`Submodules count: ${submodules.length}`);
}

main().catch(console.error);
