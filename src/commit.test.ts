import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { expect, test, describe, beforeEach, afterEach } from "vite-plus/test";
import { Repository, Commit } from "../dist/index.js";

// Standard Git SHA1 OID for an empty tree
const EMPTY_TREE_ID = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

describe("Commit creation, signature inspection, parent resolution, and verification/signing", () => {
  let tmpDir: string;

  beforeEach(() => {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "commit-test-"));
  });

  afterEach(() => {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  });

  test("Commit object captures author and committer signatures", () => {
    const repoPath = path.join(tmpDir, "sig-repo");
    const repo = Repository.init(repoPath, false);

    const commitId = repo.commit(
      "refs/heads/main",
      "Signed commit test",
      EMPTY_TREE_ID,
      []
    );

    const commit = repo.findCommit(commitId);
    expect(commit).toBeInstanceOf(Commit);
    expect(commit.id).toBe(commitId);
    expect(commit.message).toBe("Signed commit test");

    // Verify author and committer signatures
    const author = commit.author;
    expect(author).toBeDefined();
    expect(typeof author.name).toBe("string");
    expect(typeof author.email).toBe("string");
    expect(typeof author.time).toBe("number");

    const committer = commit.committer;
    expect(committer).toBeDefined();
    expect(typeof committer.name).toBe("string");
  });

  test("Commit parent traversal resolves parent Commit instances", () => {
    const repoPath = path.join(tmpDir, "parent-repo");
    const repo = Repository.init(repoPath, false);

    const parentId = repo.commit(
      "refs/heads/main",
      "Initial parent commit",
      EMPTY_TREE_ID,
      []
    );

    const childId = repo.commit(
      "refs/heads/main",
      "Child commit",
      EMPTY_TREE_ID,
      [parentId]
    );

    const childCommit = repo.findCommit(childId);
    expect(childCommit.parentIds).toEqual([parentId]);

    const parents = childCommit.parents();
    expect(parents.length).toBe(1);
    expect(parents[0]).toBeInstanceOf(Commit);
    expect(parents[0].id).toBe(parentId);
    expect(parents[0].message).toBe("Initial parent commit");
  });

  test("Commit tree resolution returns a Tree instance", () => {
    const repoPath = path.join(tmpDir, "tree-repo");
    const repo = Repository.init(repoPath, false);

    const commitId = repo.commit(
      "refs/heads/main",
      "Commit with tree",
      EMPTY_TREE_ID,
      []
    );

    const commit = repo.findCommit(commitId);
    expect(commit.treeId).toBe(EMPTY_TREE_ID);

    const tree = commit.tree();
    expect(tree).toBeDefined();
    expect(tree.id).toBe(EMPTY_TREE_ID);
  });

  test("Verifying commit signature or payload structure", () => {
    const repoPath = path.join(tmpDir, "verify-repo");
    const repo = Repository.init(repoPath, false);

    const commitId = repo.commit(
      "refs/heads/main",
      "Commit for verification",
      EMPTY_TREE_ID,
      []
    );

    const commit = repo.findCommit(commitId);

    // Draft verification check: ensure commit ID matches SHA format and commit header data is intact
    expect(commit.id).toMatch(/^[0-9a-f]{40}$/);
    expect(commit.author.time).toBeGreaterThan(0);
    expect(commit.committer.time).toBeGreaterThan(0);
  });
});
