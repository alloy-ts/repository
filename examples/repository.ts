import { Repository } from "../src/main.ts";

function main(): void {
  console.log("=== Repository API Example ===\n");

  // 1. Open the repository
  const repo = Repository.open(process.cwd());
  console.log(`Repository path: ${repo.workDir() ?? repo.gitDir()}`);
  console.log(`Is bare: ${repo.isBare()}`);
  console.log(`Object hash: ${repo.objectHash()}\n`);

  // 2. Reading Git Revisions
  console.log("--- Revisions ---");
  const headId = repo.headId();
  console.log(`HEAD commit OID: ${headId}`);

  const parsedHead = repo.revParseSingle("HEAD");
  console.log(`revParseSingle('HEAD'): ${parsedHead}`);

  const walked = repo.revWalk([headId]);
  console.log(`Reachable commits from HEAD (count): ${walked.length}`);

  const base = repo.mergeBase(headId, headId);
  console.log(`Merge base of HEAD with itself: ${base}\n`);

  // 3. Reading Git Logs
  console.log("--- Git Log (Latest 3 commits) ---");
  const logs = repo.log("HEAD", 3);
  for (const entry of logs) {
    console.log(`Commit: ${entry.id.slice(0, 7)}`);
    console.log(`Author: ${entry.authorName} <${entry.authorEmail}>`);
    console.log(`Date:   ${new Date(entry.authorTime * 1000).toISOString()}`);
    console.log(`Subject: ${entry.subject}`);
    console.log(`Parents: ${entry.parentIds.join(", ") || "(none)"}\n`);
  }

  // 4. Reading Reflog via Repository
  console.log("--- Reflog (HEAD via repo.reflog()) ---");
  try {
    const reflogs = repo.reflog("HEAD");
    console.log(`Reflog entries count for HEAD: ${reflogs.length}`);
    for (const entry of reflogs.slice(-3)) {
      console.log(
        `  [${entry.previousOid.slice(0, 7)} -> ${entry.newOid.slice(0, 7)}] ${entry.message}`,
      );
    }
  } catch (e: unknown) {
    console.log(`Reflog not available for HEAD: ${String(e)}`);
  }
}

main();
