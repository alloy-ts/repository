import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { expect, test } from "vite-plus/test";
import { Repository, type ReflogEntry } from "./main.ts";

/**
 * JS Examples & Tests for Reference Reflog:
 *
 * 1. Checking Reflog Existence:
 *    ```ts
 *    const mainRef = repo.findReference("refs/heads/main");
 *    const hasReflog = mainRef.hasLog();
 *    ```
 *
 * 2. Reading Reflog Entries:
 *    ```ts
 *    const entries: ReflogEntry[] = mainRef.log();
 *    for (const e of entries) {
 *      console.log(e.previousId, "->", e.newId, e.message);
 *    }
 *    ```
 */

function setupRepoWithReflogs(): {
  repo: Repository;
  commit1Id: string;
  commit2Id: string;
  cleanup: () => void;
} {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "repo-test-reflog-"));
  const repo = Repository.init(tmpDir, false);

  const headTree = Repository.open(process.cwd()).headCommit().treeId;

  // Create initial commit
  const commit1Id = repo.commit("refs/heads/main", "commit 1: initial setup", headTree, []);

  // Create second commit on the same branch to generate a second reflog entry
  const commit2Id = repo.commit("refs/heads/main", "commit 2: updated setup", headTree, [
    commit1Id,
  ]);

  const cleanup = () => {
    try {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    } catch {}
  };

  return { repo, commit1Id, commit2Id, cleanup };
}

// --- 1. Reflog Existence & Reading on References -----------------------------

test("ref.hasLog() and ref.log() inspect reference reflogs", () => {
  const { repo, commit1Id, commit2Id, cleanup } = setupRepoWithReflogs();
  try {
    const mainRef = repo.findReference("refs/heads/main");

    expect(mainRef.hasLog()).toBe(true);

    const logs: ReflogEntry[] = mainRef.log();
    expect(Array.isArray(logs)).toBe(true);
    expect(logs.length).toBeGreaterThanOrEqual(1);

    const newest = logs[0]!;
    expect(newest.newId).toBe(commit2Id);
    expect(newest.previousId).toBe(commit1Id);
    expect(typeof newest.committerName).toBe("string");
    expect(typeof newest.committerEmail).toBe("string");
    expect(typeof newest.committerTime).toBe("number");
    expect(typeof newest.message).toBe("string");
  } finally {
    cleanup();
  }
});

test("HEAD reflog can be read from workspace repo if present", () => {
  const repo = Repository.open(process.cwd());
  const headRef = repo.findReference("HEAD");
  if (headRef.hasLog()) {
    const logs = headRef.log();
    expect(Array.isArray(logs)).toBe(true);
  }
});
