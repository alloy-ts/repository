import { execSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Commit, Repository, Worktree } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let repo: Repository;

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-worktree-"));
  const cloneDir = join(base, "clone");
  try {
    repo = await Repository.clone(realRoot, cloneDir);
  } catch {
    rmSync(cloneDir, { recursive: true, force: true });
    execSync(`git clone --local --quiet ${JSON.stringify(realRoot)} ${JSON.stringify(cloneDir)}`, {
      stdio: "ignore",
    });
    repo = Repository.open(cloneDir);
  }
});

afterAll(() => {
  rmSync(base, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 200,
  });
});

describe("Worktree (src/worktree.rs)", () => {
  let head: Commit;
  let wt: Worktree;
  let wtLock: Worktree;
  let wtPath: string;
  let wtLockPath: string;

  beforeAll(() => {
    head = repo.headCommit();
    wtPath = join(base, "wt");
    wtLockPath = join(base, "wt-lock");
    // Synchronous, detached-HEAD constructors avoid the "branch already checked out" conflict.
    wt = new Worktree(wtPath, head, false);
    wtLock = new Worktree(wtLockPath, head, true);
  });

  test("constructor exposes id / path / gitDir", () => {
    expect(wt.id).toBe("wt");
    expect(wt.path).toBe(wtPath);
    expect(wt.gitDir.endsWith("worktrees/wt")).toBe(true);
    expect(wt.isMain).toBe(false);
  });

  test("open() returns an existing worktree and null for unknown id", () => {
    const opened = Worktree.open(wt.id, repo);
    expect(opened).not.toBeNull();
    expect(opened!.id).toBe(wt.id);
    expect(Worktree.open("does-not-exist", repo)).toBeNull();
  });

  test("validate() passes for a live worktree", () => {
    expect(() => wt.validate()).not.toThrow();
  });

  test("isPrunable() is false while the working tree exists", () => {
    expect(wt.isPrunable()).toBe(false);
  });

  test("prune() is a no-op while the working tree exists, then prunes once removed", () => {
    expect(wt.prune()).toEqual([]);
    // Simulate a stale worktree: remove the working tree, keep the admin dir.
    rmSync(wtPath, { recursive: true, force: true });
    expect(wt.isPrunable()).toBe(true);
    expect(wt.prune()).toEqual([wt.id]);
    // The admin dir is gone, so the worktree is no longer openable.
    expect(Worktree.open(wt.id, repo)).toBeNull();
  });

  test("lock-on-create locks the worktree", () => {
    expect(wtLock.isLocked()).toBe(true);
    wtLock.unlock();
    expect(wtLock.isLocked()).toBe(false);
  });

  test("pruneAll() honors the locked/valid options", () => {
    const id = wtLock.id;
    expect(Worktree.pruneAll(repo, {})).not.toContain(id);
    expect(Worktree.pruneAll(repo, { locked: true })).not.toContain(id);
    expect(Worktree.pruneAll(repo, { locked: true, valid: true })).toContain(id);
  });

  test("addWorktree resolves to a Worktree and exposes its Repository", async () => {
    const path = join(base, "wt-add");
    const added = await repo.addWorktree(path, "detached", head.id, false);
    expect(added).toBeInstanceOf(Worktree);
    expect(added.id).toBe("wt-add");
    expect(added.path).toBe(path);
    expect(added.gitDir.endsWith("worktrees/wt-add")).toBe(true);
    expect(added.isMain).toBe(false);
    const wtRepo = added.repository();
    expect(wtRepo).toBeInstanceOf(Repository);
    await added.remove();
  });

  test("addWorktree indexes the checkout as a prolly tree", async () => {
    const path = join(base, "wt-prolly");
    const added = await repo.addWorktree(path, "detached", head.id, false);
    // Every added worktree carries a prolly snapshot (root node id, never null).
    expect(added.prollyRoot).not.toBeNull();
    // O(log n) path lookup over the index — no tree objects read.
    const entry = added.prollyEntry("src/worktree.rs");
    expect(entry).not.toBeNull();
    expect(entry!.path).toBe("src/worktree.rs");
    expect(entry!.mode).toBe("100644");
    expect(added.prollyEntry("does/not/exist")).toBeNull();
    // Full listing is byte-ascending by path and mirrors the checkout.
    const entries = added.prollyEntries();
    expect(entries.length).toBeGreaterThan(0);
    const paths = entries.map((e) => e.path);
    expect(paths).toEqual([...paths].sort());
    expect(paths).toContain("src/worktree.rs");
    // Removing the worktree GCs its prolly snapshot too.
    await added.remove();
    expect(added.prollyRoot).toBeNull();
    expect(added.prollyEntries()).toEqual([]);
  });
});
