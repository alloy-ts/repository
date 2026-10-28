import { execSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, beforeEach, describe, expect, test } from "vite-plus/test";
import { Repository, ResetMode } from "./main.ts";

let fixture: string;
let repo: Repository;
let baseId: string;
let tipId: string;

function git(args: string) {
  execSync(`git ${args}`, { cwd: fixture, stdio: "ignore" });
}
function rev(spec: string): string {
  return execSync(`git rev-parse ${spec}`, { cwd: fixture }).toString().trim();
}
function write(name: string, content: string) {
  writeFileSync(join(fixture, name), content);
}
function read(name: string): string {
  return readFileSync(join(fixture, name), "utf8");
}
// Whether `name` is currently tracked in the index.
function tracked(name: string): boolean {
  const out = execSync(`git ls-files -- ${name}`, { cwd: fixture }).toString().split("\n");
  return out.includes(name);
}
// Paths that differ between the index and HEAD (i.e. staged changes).
function staged(): string[] {
  return execSync(`git diff --cached --name-only`, { cwd: fixture })
    .toString()
    .split("\n")
    .filter(Boolean);
}
// The blob content of `name` as it currently sits in the index.
function indexContent(name: string): string {
  return execSync(`git show :${name}`, { cwd: fixture }).toString();
}

beforeAll(() => {
  fixture = mkdtempSync(join(tmpdir(), "repo-reset-"));
  git("init --initial-branch=main");
  git("config user.email test@example.com");
  git("config user.name Test");
  git("config commit.gpgsign false");

  // base commit: a.txt = "base"
  write("a.txt", "base\n");
  git("add a.txt");
  git("commit -m base");

  // tip commit: a.txt = "tip" (modified) and b.txt = "b" (added)
  write("a.txt", "tip\n");
  write("b.txt", "b\n");
  git("add a.txt b.txt");
  git("commit -m tip");

  git("tag base");
  git("tag tip");
  baseId = rev("base");
  tipId = rev("tip");

  repo = Repository.open(fixture);
});

afterAll(() => {
  rmSync(fixture, { recursive: true, force: true });
});

// Restore a clean tip state before every test so mutating resets don't leak.
beforeEach(() => {
  git(`reset --hard ${tipId}`);
  git("clean -fd");
});

describe("Repository.resetTo (src/reset.rs)", () => {
  test("resolves a revision spec and returns the new HEAD SHA (Mixed by default-of-use)", async () => {
    const newId = await repo.resetTo("HEAD~1", ResetMode.Mixed);
    expect(newId).toMatch(/^[0-9a-f]{40}$/);
    expect(newId).toBe(baseId);
    expect(repo.headId()).toBe(baseId);
  });

  test("Mixed resets the index to the commit's tree but keeps the worktree", async () => {
    await repo.resetTo("HEAD~1", ResetMode.Mixed);
    // Worktree is untouched by a mixed reset.
    expect(read("a.txt")).toBe("tip\n");
    // Index now matches base, so there are no staged changes...
    expect(staged()).not.toContain("a.txt");
    // ...and b.txt (added at tip) is gone from the index but still on disk.
    expect(tracked("b.txt")).toBe(false);
    expect(read("b.txt")).toBe("b\n");
  });

  test("Soft moves HEAD only; index and worktree are kept", async () => {
    await repo.resetTo("HEAD~1", ResetMode.Soft);
    expect(repo.headId()).toBe(baseId);
    // The index still reflects tip, so vs the new HEAD the tip change is staged.
    expect(read("a.txt")).toBe("tip\n");
    expect(staged().sort()).toEqual(["a.txt", "b.txt"]);
  });

  test("Hard resets index and worktree, but preserves untracked files (bug fix)", async () => {
    write("untracked.txt", "keep me\n");
    expect(tracked("untracked.txt")).toBe(false);

    await repo.resetTo("HEAD~1", ResetMode.Hard);
    expect(repo.headId()).toBe(baseId);
    // Tracked file content rolled back to base.
    expect(read("a.txt")).toBe("base\n");
    // b.txt did not exist at base: removed from worktree and index.
    expect(() => read("b.txt")).toThrow();
    expect(tracked("b.txt")).toBe(false);
    // The untracked file survives the hard reset (the old remove_dir_all bug).
    expect(read("untracked.txt")).toBe("keep me\n");
  });

  test("also accepts a raw object id as the revision", async () => {
    const newId = await repo.resetTo(baseId, ResetMode.Mixed);
    expect(newId).toBe(baseId);
    expect(repo.headId()).toBe(baseId);
  });

  test("throws on an unresolvable revision", async () => {
    await expect(repo.resetTo("no-such-ref-xyz", ResetMode.Mixed)).rejects.toThrow();
  });

  test("resetHard() performs a hard reset to HEAD", async () => {
    write("a.txt", "modified in worktree\n");
    await repo.resetHard();
    expect(read("a.txt")).toBe("tip\n");
  });
});

describe("Repository.resetPaths (src/reset.rs)", () => {
  test("stages a path to the target commit's content (worktree untouched)", async () => {
    await repo.resetPaths("HEAD~1", "a.txt");
    // a.txt is now staged with base's content...
    expect(staged()).toContain("a.txt");
    expect(indexContent("a.txt")).toBe("base\n");
    // ...while the worktree still holds tip's content.
    expect(read("a.txt")).toBe("tip\n");
    // HEAD is never moved by resetPaths.
    expect(repo.headId()).toBe(tipId);
  });

  test("drops a path from the index when the target lacks it", async () => {
    await repo.resetPaths("HEAD~1", "b.txt");
    // b.txt exists at tip but not at HEAD~1, so it leaves the index...
    expect(tracked("b.txt")).toBe(false);
    // ...yet remains on disk as an untracked file, and HEAD is unmoved.
    expect(read("b.txt")).toBe("b\n");
    expect(repo.headId()).toBe(tipId);
  });

  test("accepts an array of paths", async () => {
    await repo.resetPaths("HEAD~1", ["a.txt", "b.txt"]);
    expect(indexContent("a.txt")).toBe("base\n");
    expect(tracked("b.txt")).toBe(false);
    expect(repo.headId()).toBe(tipId);
  });

  test("'.' resets every path", async () => {
    await repo.resetPaths("HEAD~1", ".");
    expect(indexContent("a.txt")).toBe("base\n");
    expect(tracked("b.txt")).toBe(false);
    expect(repo.headId()).toBe(tipId);
  });
});

describe("Repository.reset / resetDefault (backward-compatible delegates)", () => {
  test("reset(target, true) is a safe hard reset that preserves untracked files", async () => {
    write("untracked.txt", "keep\n");
    await repo.reset(baseId, true);
    expect(repo.headId()).toBe(baseId);
    expect(read("a.txt")).toBe("base\n");
    expect(read("untracked.txt")).toBe("keep\n");
  });

  test("resetDefault(target) is a mixed reset", async () => {
    await repo.resetDefault(baseId);
    expect(repo.headId()).toBe(baseId);
    expect(read("a.txt")).toBe("tip\n"); // worktree kept
    expect(tracked("b.txt")).toBe(false); // b removed from index
  });
});
