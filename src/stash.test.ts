import { execSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, beforeEach, describe, expect, test } from "vite-plus/test";
import { Repository, StashFlags } from "./main.ts";

let fixture: string;
let repo: Repository;

function git(args: string) {
  execSync(`git ${args}`, { cwd: fixture, stdio: "ignore" });
}
function write(name: string, content: string) {
  writeFileSync(join(fixture, name), content);
}
function read(name: string): string {
  return readFileSync(join(fixture, name), "utf8");
}

beforeAll(() => {
  fixture = mkdtempSync(join(tmpdir(), "repo-stash-"));
  git("init --initial-branch=main");
  git("config user.email test@example.com");
  git("config user.name Test");
  git("config commit.gpgsign false");

  write("file.txt", "initial\n");
  git("add file.txt");
  git("commit -m initial");

  repo = Repository.open(fixture);
});

afterAll(() => {
  rmSync(fixture, { recursive: true, force: true });
});

beforeEach(() => {
  git("reset --hard HEAD");
  git("clean -fd");
  repo.clearStash();
});

describe("Repository Stash API (src/stash.rs)", () => {
  test("saveStash and listStashes and getStash", () => {
    write("file.txt", "modified\n");
    const stashId = repo.saveStash({ message: "my test stash" });
    expect(stashId).not.toBeNull();
    expect(read("file.txt")).toBe("initial\n");

    const stashes = repo.listStashes();
    expect(stashes.length).toBe(1);
    expect(stashes[0].index).toBe(0);
    expect(stashes[0].message).toContain("my test stash");

    const entry = repo.getStash(0);
    expect(entry).not.toBeNull();
    expect(entry?.index).toBe(0);
    expect(entry?.message).toContain("my test stash");

    expect(repo.getStash(99)).toBeNull();
  });

  test("applyStash and dropStash", () => {
    write("file.txt", "stashed work\n");
    repo.saveStash({ message: "work to restore" });
    expect(read("file.txt")).toBe("initial\n");

    const outcome = repo.applyStash(0);
    expect(outcome.applied).toBe(true);
    expect(read("file.txt")).toBe("stashed work\n");

    repo.dropStash(0);
    expect(repo.listStashes().length).toBe(0);
  });

  test("popStash applies and drops the stash", () => {
    write("file.txt", "popped work\n");
    repo.saveStash({ message: "pop test" });

    const outcome = repo.popStash(0);
    expect(outcome.applied).toBe(true);
    expect(read("file.txt")).toBe("popped work\n");
    expect(repo.listStashes().length).toBe(0);
  });
});
