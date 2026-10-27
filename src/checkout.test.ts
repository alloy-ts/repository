import { execSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository } from "./main.ts";

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

beforeAll(() => {
  fixture = mkdtempSync(join(tmpdir(), "repo-checkout-"));
  git("init --initial-branch=main");
  git("config user.email test@example.com");
  git("config user.name Test");
  git("config commit.gpgsign false");

  write("a.txt", "base\n");
  git("add a.txt");
  git("commit -m base");
  baseId = rev("HEAD");

  write("a.txt", "tip\n");
  git("add a.txt");
  git("commit -m tip");
  tipId = rev("HEAD");

  repo = Repository.open(fixture);
});

afterAll(() => {
  rmSync(fixture, { recursive: true, force: true });
});

describe("Repository checkout restore (src/checkout.rs)", () => {
  test("checkoutHead() restores the worktree to HEAD without moving HEAD", async () => {
    write("a.txt", "dirty\n");
    await repo.checkoutHead({ force: true });
    expect(read("a.txt")).toBe("tip\n");
    // HEAD must be untouched by the restore.
    expect(repo.headId()).toBe(tipId);
  });

  test("checkoutTree() restores from an arbitrary tree without moving HEAD", async () => {
    // Restore the worktree to the base commit's tree while staying on tip.
    write("a.txt", "dirty\n");
    await repo.checkoutTree(baseId, { force: true });
    expect(read("a.txt")).toBe("base\n");
    // HEAD is unchanged.
    expect(repo.headId()).toBe(tipId);
  });

  test("checkoutIndex() restores the worktree from the index", async () => {
    // An unstaged modification: the index still holds the committed content, so
    // checkoutIndex() reverts the file.
    write("a.txt", "dirty\n");
    await repo.checkoutIndex({ force: true });
    expect(read("a.txt")).toBe("tip\n");
  });

  test("checkoutHead() honours paths", async () => {
    writeFileSync(join(fixture, "b.txt"), "b\n");
    git("add b.txt");
    git("commit -m add-b");

    write("a.txt", "dirty-a\n");
    write("b.txt", "dirty-b\n");
    await repo.checkoutHead({ force: true, paths: ["a.txt"] });
    // a.txt reverts to the committed content; b.txt is left dirty.
    expect(read("a.txt")).toBe("tip\n");
    expect(read("b.txt")).toBe("dirty-b\n");
  });
});
