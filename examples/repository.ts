import { Repository } from "../src/main.ts";

/**
 * Example: Using `Repository` with Node.js / TypeScript bindings
 *
 * This example demonstrates:
 *   1. Opening / discovering repositories
 *   2. Reading git logs (commit history)
 *   3. Reading reflogs
 *   4. Resolving revisions and reading file content at specific revisions (e.g. ReadMe.md from main)
 *   5. Working with commits, trees, blobs, annotated tags, and references
 *   6. Inspecting worktrees and status
 *   7. Setting up remotes from repository.manifest.yml
 */

async function main() {
  console.log("=== Repository API Examples ===\n");

  // 1. Open or discover a repository
  const repo = Repository.open(process.cwd());
  console.log(`Repository path: ${repo.workDir() ?? repo.gitDir()}`);
  console.log(`Is bare: ${repo.isBare()}`);
  console.log(`Is dirty: ${repo.isDirty()}`);
  console.log(`Object hash: ${repo.objectHash()}\n`);

  // 2. Reading Git Logs
  console.log("--- 2. Reading Git Logs ---");
  const logs = repo.log(undefined, 5); // top 5 commits
  for (const entry of logs) {
    const date = new Date(entry.authorTime * 1000).toISOString().slice(0, 10);
    console.log(`${entry.id.slice(0, 8)} ${date} ${entry.authorName}: ${entry.subject}`);
  }
  console.log();

  // 3. Reading Reflogs
  console.log("--- 3. Reading Reflogs ---");
  const reflogs = repo.headReflog();
  console.log(`Found ${reflogs.length} reflog entries for HEAD`);
  for (const r of reflogs.slice(0, 3)) {
    console.log(
      `${r.previousId.slice(0, 8)} -> ${r.newId.slice(0, 8)} (${r.committerName}): ${r.message}`,
    );
  }
  console.log();

  // 4. Resolving Revisions & Reading File Content
  console.log("--- 4. Revisions & Getting File Content ---");

  // A. Get latest ReadMe.md from current HEAD / main branch using revision spec
  try {
    const readmeBlobId = repo.revParseSingle("HEAD:ReadMe.md");
    const blob = repo.findBlob(readmeBlobId);
    const content = blob.content().toString("utf-8");
    console.log("Latest ReadMe.md title line:");
    console.log(content.split("\n")[0]);
  } catch (err) {
    console.log("Could not resolve HEAD:ReadMe.md:", err);
  }

  // B. Get file revision by specific SHA
  const headCommit = repo.headCommit();
  const shaSpec = `${headCommit.id}:ReadMe.md`;
  const blobIdFromSha = repo.revParseSingle(shaSpec);
  console.log(`Resolved ${shaSpec} -> blob ${blobIdFromSha.slice(0, 8)}\n`);

  // 5. Inspecting Commits, Trees, and Entries
  console.log("--- 5. Commit & Tree Inspection ---");
  const commit = repo.headCommit();
  console.log(`HEAD commit message: ${commit.message.trim()}`);

  const tree = commit.tree();
  console.log(`Tree ${tree.id.slice(0, 8)} entries count: ${tree.entries.length}`);

  for (const entry of tree.entries.slice(0, 5)) {
    console.log(`  ${entry.mode.padEnd(7)} ${entry.id.slice(0, 8)} ${entry.name}`);
  }
  console.log();

  // 6. Inspecting Worktrees
  console.log("--- 6. Worktree Inspection ---");
  console.log(`Is worktree: ${repo.isWorktree()}`);
  const worktrees = repo.worktrees();
  console.log(`Total worktrees: ${worktrees.length}`);
  for (const wt of worktrees) {
    console.log(`  - Worktree dir: ${wt.workDir} (isMain: ${wt.isMain})`);
  }
  console.log();

  // 7. Manifest Remote Setup (if repository.manifest.yml exists)
  console.log("--- 7. Setting up Remotes from Manifest ---");
  try {
    const remotes = repo.setupRemotesFromManifest();
    console.log("Configured remotes:");
    for (const r of remotes) {
      console.log(`  ${r.name.padEnd(12)} -> ${r.url}`);
    }
  } catch (e) {
    console.log("Manifest setup skipped:", e);
  }

  console.log("\n=== Example Completed Successfully ===");
}

main().catch(console.error);
