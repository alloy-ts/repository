import { execSync } from "node:child_process";
import { mkdtempSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository, WorktreeHeadKind, WorktreeRemoveForce } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-worktree-"));
  cloneDir = join(base, "clone");
  try {
    repo = await Repository.clone(realRoot, cloneDir, false);
  } catch (e) {
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

describe("Repository worktree family (Batch B)", () => {
  test("worktrees() lists no linked worktrees; worktree() describes the main one", () => {
    expect(repo.worktrees()).toEqual([]);
    const wt = repo.worktree();
    expect(wt?.base).toBe(cloneDir);
    expect(wt?.isMain).toBe(true);
    expect(wt?.dotGitExists).toBe(true);
  });

  test("worktreesIncludingMain() returns the main repository first", () => {
    const all = repo.worktreesIncludingMain();
    expect(all).toHaveLength(1);
    expect(all[0].workDir()).toBe(cloneDir);
  });

  test("worktreeProxyById() returns null for an unknown id", () => {
    expect(repo.worktreeProxyById("__no_such_worktree__")).toBe(null);
  });

  test("worktreeStream() materializes entries of the HEAD tree", async () => {
    const treeId = repo.headCommit().treeId;
    const entries = await repo.worktreeStream(treeId);
    expect(Array.isArray(entries)).toBe(true);
    expect(entries.length).toBeGreaterThan(0);
    expect(entries.some((e) => e.path.endsWith("Cargo.toml"))).toBe(true);
    for (const e of entries) {
      expect(typeof e.path).toBe("string");
      expect(typeof e.mode).toBe("string");
      expect(e.id).toMatch(/^[0-9a-f]{40}$/);
      expect(e.size === null || typeof e.size === "number").toBe(true);
    }
  });

  // NOTE: worktreeArchive() is intentionally not bound yet — gix's
  // worktree-archive feature needs gix-archive 0.38.0, which requires
  // tar ^0.4.46, unavailable in this machine's crates.io mirror.
  // See src/worktree.rs for the re-enable recipe.

  test("addWorktree(detached) then removeWorktree() round-trips", async () => {
    const dest = join(base, "wt-detached");
    const wt = await repo.addWorktree(dest, WorktreeHeadKind.Detached, repo.headId());
    // gix canonicalizes the destination (macOS: /var -> /private/var)
    const realDest = realpathSync(dest);
    expect(wt.workDir()).toBe(realDest);
    expect(wt.headId()).toBe(repo.headId());

    const proxies = repo.worktrees();
    expect(proxies).toHaveLength(1);
    expect(proxies[0].id).toBe("wt-detached");
    expect(proxies[0].base).toBe(realDest);
    expect(proxies[0].isLocked).toBe(false);
    expect(proxies[0].isPrunable).toBe(false);
    expect(proxies[0].lockReason ?? null).toBe(null);
    expect(repo.worktreeProxyById("wt-detached")?.gitDir).toBe(proxies[0].gitDir);

    await repo.removeWorktree(dest);
    expect(repo.worktrees()).toEqual([]);
    expect(repo.worktreeProxyById("wt-detached")).toBe(null);
  });

  test("addWorktree(attached) checks out a fresh branch and rejects a busy one", async () => {
    const current = repo.headName();
    if (!current) return;
    const short = current.replace(/^refs\/heads\//, "");

    // the current branch is checked out in the main worktree
    await expect(
      repo.addWorktree(join(base, "wt-busy"), WorktreeHeadKind.Attached, short),
    ).rejects.toThrow();

    // a fresh, unchecked-out branch works
    repo.createReference("refs/heads/__wt_probe__", repo.headId(), true);
    const dest = join(base, "wt-attached");
    const wt = await repo.addWorktree(dest, WorktreeHeadKind.Attached, "__wt_probe__");
    expect(wt.headName()).toBe("refs/heads/__wt_probe__");
    await repo.removeWorktree(dest);
    expect(repo.worktrees()).toEqual([]);
  });

  test("removeWorktree() refuses the main worktree and unknown targets", async () => {
    await expect(repo.removeWorktree(cloneDir, WorktreeRemoveForce.OverrideLock)).rejects.toThrow(
      /main worktree/i,
    );
    await expect(repo.removeWorktree(join(base, "__nope__"))).rejects.toThrow();
  });

  test("worktrees(), isWorktree(), and reset() work as expected", () => {
    const wts = repo.worktrees();
    expect(Array.isArray(wts)).toBe(true);
    expect(typeof repo.isWorktree()).toBe("boolean");
    expect(repo.isWorktree()).toBe(false);

    const head = repo.headId();
    repo.reset(head, false);
    expect(repo.headId()).toBe(head);
  });
});
