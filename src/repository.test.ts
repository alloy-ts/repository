import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { expect, test, describe, beforeEach, afterEach } from "vite-plus/test";
import { Repository } from "../dist/index.js";

// Standard Git SHA1 OID for an empty tree
const EMPTY_TREE_ID = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

describe("Repository static constructors and methods", () => {
  let tmpDir: string;

  beforeEach(() => {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "repo-test-"));
  });

  afterEach(() => {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  });

  test("Repository.init creates a new repository instance", () => {
    const repoPath = path.join(tmpDir, "new-repo");
    const repo = Repository.init(repoPath, false);

    expect(repo).toBeInstanceOf(Repository);
    expect(repo.isBare()).toBe(false);
    expect(repo.gitDir()).toContain(".git");
  });

  test("Repository.init creates a bare repository", () => {
    const repoPath = path.join(tmpDir, "bare-repo.git");
    const repo = Repository.init(repoPath, true);

    expect(repo).toBeInstanceOf(Repository);
    expect(repo.isBare()).toBe(true);
  });

  test("Repository.open opens an existing repository", () => {
    const repoPath = path.join(tmpDir, "open-repo");
    Repository.init(repoPath, false);

    const repo = Repository.open(repoPath);
    expect(repo).toBeInstanceOf(Repository);
    expect(repo.workDir()).toBe(repoPath);
  });

  test("Repository.discover discovers a repository from a subdirectory", () => {
    const repoPath = path.join(tmpDir, "disc-repo");
    Repository.init(repoPath, false);

    const subDir = path.join(repoPath, "a", "b");
    fs.mkdirSync(subDir, { recursive: true });

    const repo = Repository.discover(subDir);
    expect(repo).toBeInstanceOf(Repository);
    expect(repo.workDir()).toBe(repoPath);
  });

  test("Repository.clone clones a repository into a path", async () => {
    // Create a source repository with a commit
    const srcPath = path.join(tmpDir, "src-repo");
    const srcRepo = Repository.init(srcPath, false);

    const commitId = srcRepo.commit(
      "refs/heads/main",
      "Initial commit",
      EMPTY_TREE_ID,
      []
    );
    expect(commitId).toBeDefined();

    const clonePath = path.join(tmpDir, "cloned-repo");
    const clonedRepo = await Repository.clone(srcPath, clonePath, false);

    expect(clonedRepo).toBeInstanceOf(Repository);
    expect(clonedRepo.workDir()).toBe(clonePath);
  });

  test("Repository.log reads commit logs", () => {
    const repoPath = path.join(tmpDir, "log-repo");
    const repo = Repository.init(repoPath, false);

    const commit1 = repo.commit(
      "refs/heads/main",
      "First commit\n\nDetailed message",
      EMPTY_TREE_ID,
      []
    );
    const commit2 = repo.commit(
      "refs/heads/main",
      "Second commit",
      EMPTY_TREE_ID,
      [commit1]
    );

    const entries = repo.log("refs/heads/main", 10, false);
    expect(entries.length).toBe(2);
    expect(entries[0].id).toBe(commit2);
    expect(entries[0].subject).toBe("Second commit");
    expect(entries[1].id).toBe(commit1);
    expect(entries[1].subject).toBe("First commit");
  });

  test("Repository.reflog reads reflog entries from Repository", () => {
    const repoPath = path.join(tmpDir, "repo-reflog");
    const repo = Repository.init(repoPath, false);

    const commit1 = repo.commit(
      "refs/heads/main",
      "Commit 1",
      EMPTY_TREE_ID,
      []
    );
    const commit2 = repo.commit(
      "refs/heads/main",
      "Commit 2",
      EMPTY_TREE_ID,
      [commit1]
    );

    const reflog = repo.reflog("refs/heads/main");
    expect(Array.isArray(reflog)).toBe(true);
    expect(reflog.length).toBeGreaterThanOrEqual(1);
    expect(reflog[reflog.length - 1].newOid).toBe(commit2);
  });

  test("Repository.revParseSingle resolves revisions to read file contents from branch or SHA", () => {
    const repoPath = path.join(tmpDir, "rev-parse-repo");
    const repo = Repository.init(repoPath, false);

    // Create a blob for ReadMe.md content
    const readmeContent = "# Hello Repository";
    const blobId = repo.writeBlob(Buffer.from(readmeContent));

    // For testing tree resolution, verify revParseSingle resolves main and SHA
    const commitId = repo.commit(
      "refs/heads/main",
      "Add ReadMe.md",
      EMPTY_TREE_ID,
      []
    );

    // Resolve 'main' branch spec to commit SHA
    const mainOid = repo.revParseSingle("refs/heads/main");
    expect(mainOid).toBe(commitId);

    // Resolve SHA spec
    const shaOid = repo.revParseSingle(commitId);
    expect(shaOid).toBe(commitId);

    // Read stored blob content directly using its OID
    const blob = repo.findBlob(blobId);
    expect(blob.content().toString("utf-8")).toBe(readmeContent);
  });

  test("Repository.revWalk and findCommit walk revisions and resolve commits", () => {
    const repoPath = path.join(tmpDir, "revwalk-repo");
    const repo = Repository.init(repoPath, false);

    const commit1 = repo.commit(
      "refs/heads/main",
      "Commit 1",
      EMPTY_TREE_ID,
      []
    );

    const commitOids = repo.revWalk([commit1]);
    expect(commitOids).toContain(commit1);

    const resolved = repo.findCommit(commit1);
    expect(resolved.id).toBe(commit1);
    expect(resolved.message).toBe("Commit 1");
  });
});
