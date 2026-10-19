import { Repository, setupRemotes } from "../dist/index.js";

async function main() {
  console.log("=== 1. Opening Repository ===");
  const repo = Repository.discover(".");
  console.log(`Git Directory: ${repo.gitDir()}`);
  console.log(`Work Directory: ${repo.workDir()}`);
  console.log(`Is Bare: ${repo.isBare()}`);
  console.log(`HEAD ID: ${repo.headId()}`);
  console.log(`HEAD Name: ${repo.headName()}`);

  console.log("\n=== 2. Reading Git Log ===");
  const entries = repo.log(undefined, 5, false);
  for (const entry of entries) {
    const when = new Date(entry.authorTime * 1000).toISOString().slice(0, 10);
    console.log(`  ${entry.id.slice(0, 8)} | ${when} | ${entry.authorName} | ${entry.subject}`);
  }

  console.log("\n=== 3. Reading Reflog ===");
  const headReflog = repo.reflog("HEAD");
  console.log(`HEAD Reflog entries count: ${headReflog.length}`);
  const latest = headReflog[0];
  if (latest) {
    console.log(
      `  Latest reflog action: ${latest.message} (${latest.oldId.slice(0, 8)} -> ${latest.newId.slice(0, 8)})`,
    );
  }

  console.log("\n=== 4. Revisions API & Reading File Content ===");
  const headSha = repo.revParseSingle("HEAD");
  console.log(`Resolved 'HEAD': ${headSha}`);

  const headTreeId = repo.headTreeId();
  console.log(`HEAD Tree ID: ${headTreeId}`);

  // Fetch latest ReadMe.md from HEAD / branch / SHA
  const commit = repo.findCommit(headSha);
  const tree = repo.findTree(commit.treeId);
  const readmeEntry = tree.entries.find((e) => e.name === "ReadMe.md");
  if (readmeEntry) {
    const readmeBlob = repo.findBlob(readmeEntry.id);
    const content = readmeBlob.content().toString("utf8");
    console.log(
      `ReadMe.md (${readmeEntry.id.slice(0, 8)}) line count: ${content.split("\n").length}`,
    );
  }

  // Reading repository.manifest.yml via revision spec
  const manifestSha = repo.revParseSingle("HEAD:repository.manifest.yml");
  console.log(`SHA for HEAD:repository.manifest.yml: ${manifestSha}`);

  console.log("\n=== 5. Working Tree & Status ===");
  const status = repo.status();
  console.log(`Working Tree Status entries: ${status.length}`);

  console.log("\n=== 6. Remotes & Manifest Setup ===");
  const remotes = repo.remotes();
  console.log(`Configured Remotes: ${remotes.map((r) => r.name).join(", ")}`);

  try {
    const setupResult = setupRemotes(".", "HEAD");
    console.log(`setupRemotes configured: ${setupResult.join(", ")}`);
  } catch (e) {
    console.log(`setupRemotes notice: ${String(e)}`);
  }

  console.log("\n=== Example completed successfully ===");
}

main().catch(console.error);
