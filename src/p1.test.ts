import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository, WorktreeHeadKind, WorktreeRemoveForce } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

function git(args: string, cwd: string) {
  execSync(`git ${args}`, { cwd, stdio: "ignore" });
}

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-p1-"));
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
  writeFileSync(join(cloneDir, "p1_fixture.txt"), "p1 fixture content\n");
  git("add -A", cloneDir);
  git("config commit.gpgsign false", cloneDir);
  git('-c user.email=test@example.com -c user.name=Test commit -m "p1 fixture commit"', cloneDir);
  repo = Repository.open(cloneDir);
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true });
});

describe("P1 Core Operations & Worktree", () => {
  test("mainWorktree / mainRepository helpers", () => {
    const mainRepo = repo.mainRepository();
    expect(mainRepo.gitDir()).toBe(repo.gitDir());

    const mainWt = repo.mainWorktree();
    expect(mainWt).toBeDefined();
    if (mainWt) {
      expect(mainWt.isMain).toBe(true);
      expect(typeof mainWt.base).toBe("string");
    }
  });

  test("addWorktree and removeWorktree", async () => {
    const wtDir = join(base, "linked-wt");
    const headCommit = repo.headId();
    const wtRepo = await repo.addWorktree(
      wtDir,
      WorktreeHeadKind.Detached,
      headCommit
    );
    expect(wtRepo.isWorktree()).toBe(true);

    const proxies = repo.worktrees();
    expect(proxies.length).toBeGreaterThan(0);

    const wtByProxy = repo.worktreeProxyById(proxies[0].id);
    expect(wtByProxy).not.toBeNull();
    expect(wtByProxy?.id).toBe(proxies[0].id);

    await repo.removeWorktree(wtDir, WorktreeRemoveForce.DiscardChanges);
    expect(repo.worktrees().length).toBe(0);
  });
});
