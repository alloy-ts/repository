import { Repository, setupRemotes } from "../dist/index.js";

function main() {
  console.log("=== Repository API Examples ===\n");

  // 1. Open current repository
  const repo = Repository.open(".");
  console.log(`Repository opened at: ${repo.workDir() ?? repo.gitDir()}`);
  console.log(`HEAD commit ID: ${repo.headId()}`);
  console.log(`HEAD symbolic branch: ${repo.headName() ?? "(detached)"}`);

  // 2. Reading git log
  console.log("\n--- 1. Git Log (latest 3 commits) ---");
  const logEntries = repo.log(undefined, 3, false);
  for (const entry of logEntries) {
    const date = new Date(entry.authorTime * 1000).toISOString().slice(0, 10);
    console.log(`[${entry.id.slice(0, 8)}] ${date} ${entry.authorName}: ${entry.subject}`);
  }

  // 3. Reading git reflog
  console.log("\n--- 2. Git Reflog (HEAD) ---");
  const headRefName = repo.headName() ?? "HEAD";
  const reflogEntries = repo.reflog(headRefName);
  if (reflogEntries.length === 0) {
    console.log(`No reflog entries found for ${headRefName}.`);
  } else {
    for (const entry of reflogEntries.slice(0, 3)) {
      console.log(`- ${entry.oldId.slice(0, 7)} -> ${entry.newId.slice(0, 7)}: ${entry.message}`);
    }
  }

  // 4. Resolving Revisions (spec, SHA, branch, <rev>:<path>)
  console.log("\n--- 3. Resolving Revisions ---");
  const headSha = repo.revParseSingle("HEAD");
  console.log(`revParseSingle("HEAD"): ${headSha}`);

  // Resolve commit SHA directly
  const shaResolved = repo.revParseSingle(headSha);
  console.log(`revParseSingle("${headSha.slice(0, 8)}..."): ${shaResolved}`);

  // Resolve revision spec pointing to a file blob (<rev>:<path>)
  try {
    const readmeBlobId = repo.revParseSingle("HEAD:ReadMe.md");
    console.log(`revParseSingle("HEAD:ReadMe.md"): ${readmeBlobId}`);
    const blob = repo.findBlob(readmeBlobId);
    const text = blob.content().toString("utf8");
    const headline = text.split("\n")[0];
    console.log(`ReadMe.md headline: "${headline}"`);
  } catch (err) {
    console.log(`Could not resolve "HEAD:ReadMe.md": ${String(err)}`);
  }

  // 5. Manifest & Setup Remotes
  console.log("\n--- 4. Manifest & setupRemotes ---");
  try {
    const remotes = setupRemotes(".", "HEAD", "repository.manifest.yml");
    console.log(`Configured remotes from repository.manifest.yml at HEAD:`, remotes);
  } catch (err) {
    console.log(`setupRemotes skipped/failed: ${String(err)}`);
  }

  console.log("\n=== Example Completed Successfully ===");
}

main();
