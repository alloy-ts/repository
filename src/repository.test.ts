import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { expect, test, describe, beforeEach, afterEach } from "vite-plus/test";
import { Repository, Reference, Tree } from "../dist/index.js";

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

    const readmeContent = "# Hello Repository";
    const blobId = repo.writeBlob(Buffer.from(readmeContent));

    const commitId = repo.commit(
      "refs/heads/main",
      "Add ReadMe.md",
      EMPTY_TREE_ID,
      []
    );

    const mainOid = repo.revParseSingle("refs/heads/main");
    expect(mainOid).toBe(commitId);

    const shaOid = repo.revParseSingle(commitId);
    expect(shaOid).toBe(commitId);

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

  describe("Wave 1 API Additions", () => {
    test("headTreeId, headRef, headTree, and setHead", () => {
      const repoPath = path.join(tmpDir, "wave1-head-repo");
      const repo = Repository.init(repoPath, false);

      const commit1 = repo.commit(
        "refs/heads/main",
        "Commit for HEAD test",
        EMPTY_TREE_ID,
        []
      );

      expect(repo.headTreeId()).toBe(EMPTY_TREE_ID);

      const headTree = repo.headTree();
      expect(headTree).toBeInstanceOf(Tree);
      expect(headTree.id).toBe(EMPTY_TREE_ID);

      const headRef = repo.headRef();
      expect(headRef).toBeInstanceOf(Reference);
      expect(headRef?.name).toBe("refs/heads/main");

      // Test setHead to a new branch
      repo.createReference("refs/heads/feature", commit1, false);
      repo.setHead("feature");
      expect(Repository.open(repoPath).headName()).toBe("refs/heads/feature");
    });

    test("remoteAt and createRemote", () => {
      const repoPath = path.join(tmpDir, "wave1-remote-repo");
      const repo = Repository.init(repoPath, false);

      const url = "https://github.com/example/repo.git";
      const anonRemote = repo.remoteAt(url);
      expect(anonRemote.url).toBe(url);

      const createdRemote = repo.createRemote("origin", url);
      expect(createdRemote.name).toBe("origin");
      expect(createdRemote.url).toBe(url);

      const fresh = Repository.open(repoPath);
      const remotes = fresh.remotes();
      expect(remotes.some((r) => r.name === "origin")).toBe(true);
    });

    test("configEntries returns flattened config key-value pairs", () => {
      const repoPath = path.join(tmpDir, "wave1-config-repo");
      const repo = Repository.init(repoPath, false);

      const entries = repo.configEntries();
      expect(Array.isArray(entries)).toBe(true);
      expect(entries.length).toBeGreaterThan(0);
      expect(entries[0]).toHaveProperty("key");
      expect(entries[0]).toHaveProperty("value");
    });

    test("isAncestor, describe, and shallow", () => {
      const repoPath = path.join(tmpDir, "wave1-rev-repo");
      const repo = Repository.init(repoPath, false);

      const parent = repo.commit(
        "refs/heads/main",
        "Parent commit",
        EMPTY_TREE_ID,
        []
      );

      const child = repo.commit(
        "refs/heads/main",
        "Child commit",
        EMPTY_TREE_ID,
        [parent]
      );

      expect(repo.isAncestor(parent, child)).toBe(true);
      expect(repo.isAncestor(child, parent)).toBe(false);

      const desc = repo.describe(child);
      expect(desc === null || typeof desc === "string").toBe(true);

      const shallowCommits = repo.shallow();
      expect(shallowCommits === null || Array.isArray(shallowCommits)).toBe(true);
    });
  });

  describe("Wave 2 API Additions", () => {
    test("index returns entries from staging area", () => {
      const repoPath = path.join(tmpDir, "wave2-index-repo");
      const repo = Repository.init(repoPath, false);

      const indexEntries = repo.index();
      expect(Array.isArray(indexEntries)).toBe(true);
    });
  });
});
