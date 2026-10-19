import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { expect, test, describe, beforeEach, afterEach } from "vite-plus/test";
import { Repository } from "../dist/index.js";

const EMPTY_TREE_ID = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

describe("Reference methods and reflog operations", () => {
  let tmpDir: string;

  beforeEach(() => {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "ref-test-"));
  });

  afterEach(() => {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  });

  test("reads reference reflog entries", () => {
    const repoPath = path.join(tmpDir, "reflog-repo");
    const repo = Repository.init(repoPath, false);

    const c1 = repo.commit(
      "refs/heads/main",
      "Commit 1 for reflog",
      EMPTY_TREE_ID,
      []
    );

    const c2 = repo.commit(
      "refs/heads/main",
      "Commit 2 for reflog",
      EMPTY_TREE_ID,
      [c1]
    );

    const mainRef = repo.findReference("refs/heads/main");
    expect(mainRef).toBeDefined();

    const reflog = mainRef.reflog();
    expect(Array.isArray(reflog)).toBe(true);
    expect(reflog.length).toBeGreaterThanOrEqual(1);

    const latest = reflog[reflog.length - 1];
    expect(latest.newOid).toBe(c2);
    expect(typeof latest.message).toBe("string");
    expect(latest.committer).toBeDefined();
  });

  test("peels reference and deletes reference", () => {
    const repoPath = path.join(tmpDir, "ref-ops-repo");
    const repo = Repository.init(repoPath, false);

    const c1 = repo.commit(
      "refs/heads/main",
      "Initial commit",
      EMPTY_TREE_ID,
      []
    );

    const branchRef = repo.createReference("refs/heads/feature", c1, false);
    expect(branchRef.peel()).toBe(c1);

    branchRef.delete();
    expect(() => repo.findReference("refs/heads/feature")).toThrow();
  });
});
