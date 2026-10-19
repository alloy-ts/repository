import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { expect, test, describe, beforeEach, afterEach } from "vite-plus/test";
import { Repository, Tag } from "../dist/index.js";

const EMPTY_TREE_ID = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

describe("Tags, annotated objects, commits, and refs", () => {
  let tmpDir: string;

  beforeEach(() => {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "tag-test-"));
  });

  afterEach(() => {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  });

  test("creates an annotated tag pointing to a commit", () => {
    const repoPath = path.join(tmpDir, "annotated-tag-repo");
    const repo = Repository.init(repoPath, false);

    const commitId = repo.commit(
      "refs/heads/main",
      "Initial commit for tagging",
      EMPTY_TREE_ID,
      []
    );

    // Create an annotated tag 'v1.0.0' pointing to the commit
    const tagRef = repo.createTag(
      "v1.0.0",
      commitId,
      "Release version 1.0.0",
      false
    );

    expect(tagRef).toBeDefined();
    expect(tagRef.name).toBe("refs/tags/v1.0.0");
    expect(tagRef.target).toBeDefined();

    // Find and inspect the created Tag object
    const tagObj = repo.findTag(tagRef.target!);
    expect(tagObj).toBeInstanceOf(Tag);
    expect(tagObj.name).toBe("v1.0.0");
    expect(tagObj.targetId).toBe(commitId);
    expect(tagObj.message).toBe("Release version 1.0.0");

    // Verify peeled reference resolves to the target commit
    const peeledId = tagRef.peel();
    expect(peeledId).toBe(commitId);
  });

  test("creates a lightweight tag pointing directly to a commit", () => {
    const repoPath = path.join(tmpDir, "lightweight-tag-repo");
    const repo = Repository.init(repoPath, false);

    const commitId = repo.commit(
      "refs/heads/main",
      "Commit for lightweight tag",
      EMPTY_TREE_ID,
      []
    );

    const tagRef = repo.createLightweightTag("v0.1.0-alpha", commitId, false);
    expect(tagRef.name).toBe("refs/tags/v0.1.0-alpha");
    expect(tagRef.target).toBe(commitId);
    expect(tagRef.peel()).toBe(commitId);
  });

  test("queries all references and finds reference by name", () => {
    const repoPath = path.join(tmpDir, "refs-repo");
    const repo = Repository.init(repoPath, false);

    const commitId = repo.commit(
      "refs/heads/main",
      "Commit for refs query",
      EMPTY_TREE_ID,
      []
    );

    repo.createLightweightTag("v1.1", commitId, false);

    const refs = repo.references();
    expect(refs.length).toBeGreaterThan(0);

    const foundRef = repo.findReference("refs/tags/v1.1");
    expect(foundRef).toBeDefined();
    expect(foundRef.target).toBe(commitId);
  });
});
