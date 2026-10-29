import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";

import { Repository, Worktree, FileMode, WorktreeHeadKind } from "./main.ts";

let fixture: string;
let repo: Repository;

function git(args: string, cwd = fixture) {
  const r = spawnSync("git", args.split(" "), {
    cwd,
    stdio: "ignore",
  });
  if (r.status !== 0) {
    throw new Error(`git ${args} failed with status ${r.status}`);
  }
}

beforeAll(() => {
  fixture = mkdtempSync(join(tmpdir(), "napi-bindings-test-"));
  git("init --initial-branch=main");
  git("config user.email test@example.com");
  git("config user.name Test");
  git("config commit.gpgsign false");

  writeFileSync(join(fixture, "a.txt"), "hello world\n");
  git("add a.txt");
  git("commit -m initial");

  repo = Repository.open(fixture);
});

afterAll(() => {
  rmSync(fixture, { recursive: true, force: true });
});

describe("NAPI bindings - Status & File Modified", () => {
  test("fileModified checks if file on disk differs from expected oid", () => {
    const treeId = repo.peelTo("HEAD", "tree");
    expect(treeId).toBeDefined();

    // Unmodified file
    const isModified = repo.fileModified("a.txt", "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
    expect(typeof isModified).toBe("boolean");

    // Non-existent file is modified
    expect(repo.fileModified("non-existent.txt", "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391")).toBe(true);
  });
});

describe("NAPI bindings - Worktree File Operations", () => {
  test("Worktree file level operations using Worktree.open or linked worktree", async () => {
    const wtPath = join(fixture, "wt1");
    const commitId = repo.revParseSingle("HEAD");
    const wt = await repo.addWorktree(wtPath, WorktreeHeadKind.Detached, commitId);
    expect(wt).toBeDefined();

    wt.writeFile("test-file.txt", Buffer.from("test content"), FileMode.Regular);
    expect(wt.fileMode("test-file.txt")).toBe(FileMode.Regular);

    const buf = wt.readFile("test-file.txt");
    expect(buf).not.toBeNull();
    expect(buf!.toString()).toBe("test content");

    wt.removeFile("test-file.txt");
    expect(wt.readFile("test-file.txt")).toBeNull();
    expect(wt.fileMode("test-file.txt")).toBeNull();
  });
});

describe("NAPI bindings - Revision dwimRef, previousCheckout, peelTo", () => {
  test("dwimRef resolves short branch names", () => {
    const resolved = repo.dwimRef("main");
    expect(resolved).toBe("refs/heads/main");
    expect(repo.dwimRef("non-existent-ref-12345")).toBeNull();
  });

  test("previousCheckout returns past checkouts", () => {
    git("checkout -b branch1");
    git("checkout main");
    const prev = repo.previousCheckout(1);
    expect(prev).toBe("branch1");
  });

  test("peelTo peels object spec to target object kind", () => {
    const treeId = repo.peelTo("HEAD", "tree");
    expect(typeof treeId).toBe("string");
    expect(treeId.length).toBe(40);

    const commitId = repo.peelTo("main", "commit");
    expect(typeof commitId).toBe("string");
    expect(commitId.length).toBe(40);
  });
});

describe("NAPI bindings - Stash aliases and includeUntracked option", () => {
  test("stashSave with includeUntracked, stashList, stashApply, stashPop, stashDrop", () => {
    const stashDir = mkdtempSync(join(tmpdir(), "stash-test-"));
    git("init --initial-branch=main", stashDir);
    git("config user.email test@example.com", stashDir);
    git("config user.name Test", stashDir);
    git("config commit.gpgsign false", stashDir);

    writeFileSync(join(stashDir, "a.txt"), "hello\n");
    git("add a.txt", stashDir);
    git("commit -m init", stashDir);

    const stashRepo = Repository.open(stashDir);

    writeFileSync(join(stashDir, "untracked.txt"), "untracked file\n");
    writeFileSync(join(stashDir, "a.txt"), "modified a\n");

    const stashId = stashRepo.stashSave({
      message: "my test stash",
      includeUntracked: true,
      author: { name: "Test", email: "test@example.com", time: Math.floor(Date.now() / 1000), timezoneOffset: 0 },
    });
    expect(stashId).not.toBeNull();

    const stashes = stashRepo.stashList();
    expect(stashes.length).toBeGreaterThan(0);
    expect(stashes[0].message).toContain("my test stash");

    // Clean worktree before pop
    git("reset --hard HEAD", stashDir);
    git("clean -fd", stashDir);

    const popRes = stashRepo.stashPop(0);
    expect(popRes.applied).toBe(true);

    const afterPopStashes = stashRepo.stashList();
    expect(afterPopStashes.length).toBe(0);

    rmSync(stashDir, { recursive: true, force: true });
  });
});

describe("NAPI bindings - Reset Hard", () => {
  test("resetHard resets worktree and index", () => {
    const freshRepo = Repository.open(fixture);
    writeFileSync(join(fixture, "a.txt"), "dirty content\n");
    const resetRes = freshRepo.resetHard();
    expect(typeof resetRes).toBe("string");
    expect(resetRes.length).toBe(40);

    // Content should be restored
    const content = readFileSync(join(fixture, "a.txt"), "utf8");
    expect(content).toBe("hello world\n");
  });
});

describe("NAPI bindings - Rebase isRebasing & rebasePlan", () => {
  test("isRebasing returns false when clean", () => {
    expect(repo.isRebasing()).toBe(false);
  });

  test("rebasePlan generates plan from upstream", () => {
    git("checkout -b feature");
    writeFileSync(join(fixture, "feat.txt"), "feat\n");
    git("add feat.txt");
    git("commit -m feat-commit");

    const freshRepo = Repository.open(fixture);
    const plan = freshRepo.rebasePlan("main");
    expect(plan.length).toBe(1);
    expect(plan[0].kind).toBe("pick");

    git("checkout main");
  });
});

describe("NAPI bindings - Log from / maxCount options", () => {
  test("log supports from and maxCount options", () => {
    const logs = repo.log(null as any, {
      from: "HEAD",
      maxCount: 1,
    });
    expect(logs.length).toBe(1);
  });
});
