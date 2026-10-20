import { execSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-commit-"));
  cloneDir = join(base, "clone");
  try {
    repo = await Repository.clone(realRoot, cloneDir, false);
  } catch {
    rmSync(cloneDir, { recursive: true, force: true });
    execSync(`git clone --local --quiet ${JSON.stringify(realRoot)} ${JSON.stringify(cloneDir)}`, {
      stdio: "ignore",
    });
    repo = Repository.open(cloneDir);
  }
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true });
});

describe("Repository commits (src/commit.rs)", () => {
  test("findCommit resolves a real commit and matches headCommit", () => {
    const c = repo.headCommit();
    const again = repo.findCommit(c.id);
    expect(again.message).toBe(c.message);
    expect(again.treeId).toBe(c.treeId);
  });

  test("findCommit throws on a missing id", () => {
    expect(() => repo.findCommit("0".repeat(40))).toThrow();
  });

  test("commit handle exposes author / committer / parents", () => {
    const c = repo.findCommit(repo.headId());
    expect(c.id).toBe(repo.headId());
    expect(typeof c.message).toBe("string");
    expect(c.treeId).toMatch(/^[0-9a-f]{40}$/);
    expect(Array.isArray(c.parentIds)).toBe(true);
    expect(typeof c.author().name).toBe("string");
    expect(typeof c.committer().name).toBe("string");
    // `signature` is a getter: null for unsigned commits, a string otherwise.
    expect(c.signature === null || typeof c.signature === "string").toBe(true);
  });

  test("writeCommit() writes a loose commit readable via findCommit", () => {
    const head = repo.headCommit();
    const newCommitId = repo.writeCommit({
      tree: head.treeId,
      parents: [head.id],
      message: "custom loose commit message\n",
    });
    expect(typeof newCommitId).toBe("string");
    expect(newCommitId).toMatch(/^[0-9a-f]{40}$/);

    const commitObj = repo.findCommit(newCommitId);
    expect(commitObj.message).toBe("custom loose commit message\n");
    expect(commitObj.treeId).toBe(head.treeId);
    expect(commitObj.parentIds).toEqual([head.id]);
  });

  test("commit() creates a new commit on a throwaway ref", () => {
    const head = repo.headCommit();
    const newId = repo.commit("refs/heads/__probe_commit__", "probe commit message", head.treeId, [
      head.id,
    ]);
    expect(typeof newId).toBe("string");
    expect(newId).not.toBe(head.id);

    const c = repo.findCommit(newId);
    expect(c.message).toBe("probe commit message");
    expect(c.parentIds).toEqual([head.id]);
    expect(c.treeId).toBe(head.treeId);

    repo.findReference("refs/heads/__probe_commit__").delete();
  });

  test("commitGraph() / commitGraphIfEnabled() return null or info", () => {
    const cg = repo.commitGraph();
    expect(cg === null || typeof cg.commitCount === "number").toBe(true);
    const enabled = repo.commitGraphIfEnabled();
    expect(enabled === null || typeof enabled.commitCount === "number").toBe(true);
  });
});
