import { expect, test } from "vite-plus/test";
import { Repository } from "../dist/index.js";
import * as fs from "node:fs";
import * as path from "node:path";
import * as os from "node:os";

function createTmpDir(prefix: string): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

function removeTmpDir(dirPath: string): void {
  fs.rmSync(dirPath, { recursive: true, force: true });
}

test("Repository static constructors: init, open, discover, clone", async () => {
  const baseDir = createTmpDir("git-repo-test-");

  try {
    const repoPath = path.join(baseDir, "my-repo");
    const subDir = path.join(repoPath, "sub", "dir");

    // 1. Repository.init
    const repo = Repository.init(repoPath, false);
    expect(repo).toBeDefined();
    expect(repo.isBare()).toBe(false);
    expect(repo.gitDir()).toContain(".git");

    // Create a commit so repo has history
    const emptyTreeId = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    const commitId1 = repo.commit("refs/heads/main", "Commit 1", emptyTreeId, []);
    expect(commitId1).toBeDefined();

    // 2. Repository.open
    const openedRepo = Repository.open(repoPath);
    expect(openedRepo).toBeDefined();
    expect(openedRepo.headId()).toBe(commitId1);

    // 3. Repository.discover
    fs.mkdirSync(subDir, { recursive: true });
    const discoveredRepo = Repository.discover(subDir);
    expect(discoveredRepo).toBeDefined();
    expect(discoveredRepo.headId()).toBe(commitId1);

    // 4. Repository.clone
    const clonePath = path.join(baseDir, "cloned-repo");
    const clonedRepo = await Repository.clone(repo.gitDir(), clonePath, false);
    expect(clonedRepo).toBeDefined();
    expect(clonedRepo.headId()).toBe(commitId1);
  } finally {
    removeTmpDir(baseDir);
  }
});

test("Repository.log & revWalk: reading git logs in JS/TS", () => {
  const tmpDir = createTmpDir("git-log-test-");

  try {
    const repo = Repository.init(tmpDir, false);
    const emptyTreeId = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

    // Create a series of commits
    const c1 = repo.commit("refs/heads/main", "feat: first commit\n\nDetail line 1", emptyTreeId, []);
    const c2 = repo.commit("refs/heads/main", "fix: second commit", emptyTreeId, [c1]);
    const c3 = repo.commit("refs/heads/main", "docs: third commit", emptyTreeId, [c2]);

    // Read log via single call repo.log()
    const logEntries = repo.log(undefined, 10, false);
    expect(logEntries.length).toBe(3);

    // Check newest first
    expect(logEntries[0].id).toBe(c3);
    expect(logEntries[0].subject).toBe("docs: third commit");
    expect(logEntries[0].message).toBe("docs: third commit");
    expect(logEntries[0].parentIds).toEqual([c2]);

    expect(logEntries[1].id).toBe(c2);
    expect(logEntries[1].subject).toBe("fix: second commit");

    expect(logEntries[2].id).toBe(c1);
    expect(logEntries[2].subject).toBe("feat: first commit");
    expect(logEntries[2].message).toBe("feat: first commit\n\nDetail line 1");
    expect(logEntries[2].parentIds).toEqual([]);

    // Alternative: walk commit IDs manually using revWalk and findCommit
    const walkedIds = repo.revWalk([repo.headId()]);
    expect(walkedIds).toEqual([c3, c2, c1]);

    const commits = walkedIds.map((id) => repo.findCommit(id));
    expect(commits[0].message).toBe("docs: third commit");
    expect(commits[1].message).toBe("fix: second commit");
    expect(commits[2].message).toBe("feat: first commit\n\nDetail line 1");

    // Check reading reflogs directly from repo
    const logs = repo.reflog("refs/heads/main");
    expect(Array.isArray(logs)).toBe(true);
    expect(logs.length).toBeGreaterThan(0);
    expect(logs[0].newId).toBe(c1);
  } finally {
    removeTmpDir(tmpDir);
  }
});

test("Waves 4, 5, 6: writeCommitObject, worktreePath, mainRepo, mergeBaseOctopus", () => {
  const tmpDir = createTmpDir("git-waves456-test-");

  try {
    const repo = Repository.init(tmpDir, false);
    const emptyTreeId = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

    // 1. writeCommitObject
    const unrefCommitId = repo.writeCommitObject("unreferenced commit", emptyTreeId, []);
    expect(typeof unrefCommitId).toBe("string");
    expect(unrefCommitId.length).toBe(40);
    expect(repo.objectExists(unrefCommitId)).toBe(true);

    const fetchedCommit = repo.findCommit(unrefCommitId);
    expect(fetchedCommit.message).toBe("unreferenced commit");

    // 2. worktreePath and mainRepo
    const wtPath = repo.worktreePath();
    expect(wtPath).not.toBeNull();
    expect(typeof wtPath).toBe("string");

    const mainRepo = repo.mainRepo();
    expect(mainRepo).toBeDefined();

    // 3. mergeBaseOctopus
    const c1 = repo.commit("refs/heads/main", "c1", emptyTreeId, []);
    const c2 = repo.commit("refs/heads/branch-a", "c2", emptyTreeId, [c1]);
    const c3 = repo.commit("refs/heads/branch-b", "c3", emptyTreeId, [c1]);

    const octopusBase = repo.mergeBaseOctopus([c2, c3]);
    expect(octopusBase).toBe(c1);
  } finally {
    removeTmpDir(tmpDir);
  }
});

test("Wave 1 extensions: headRef, headTreeId, configEntries, isAncestor", () => {
  const tmpDir = createTmpDir("git-wave1-test-");

  try {
    const repo = Repository.init(tmpDir, false);
    const emptyTreeId = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

    const c1 = repo.commit("refs/heads/main", "Commit 1", emptyTreeId, []);
    const c2 = repo.commit("refs/heads/main", "Commit 2", emptyTreeId, [c1]);

    // 1. headRef and headTreeId
    const headRef = repo.headRef();
    expect(headRef).not.toBeNull();
    expect(headRef?.name).toBe("refs/heads/main");
    expect(headRef?.target).toBe(c2);

    expect(repo.headTreeId()).toBe(emptyTreeId);

    // 2. configEntries
    const configs = repo.configEntries();
    expect(Array.isArray(configs)).toBe(true);

    // 3. isAncestor
    expect(repo.isAncestor(c1, c2)).toBe(true);
    expect(repo.isAncestor(c2, c1)).toBe(false);
    expect(repo.isAncestor(c1, c1)).toBe(true);
  } finally {
    removeTmpDir(tmpDir);
  }
});
