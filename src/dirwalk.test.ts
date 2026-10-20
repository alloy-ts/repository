import { execSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";

import { Repository } from "./main.ts";

let fixture: string;
let repo: Repository;

function git(args: string, cwd: string) {
  execSync(`git ${args}`, { cwd, stdio: "ignore" });
}

function writeFileWithDirs(path: string, contents: string) {
  mkdirSync(join(path, ".."), { recursive: true });
  writeFileSync(path, contents);
}

beforeAll(() => {
  fixture = mkdtempSync(join(tmpdir(), "repository-dirwalk-"));
  git("init --initial-branch=main", fixture);
  git("config user.email test@example.com", fixture);
  git("config user.name Test", fixture);
  // The dev machine signs commits globally; the throwaway fixture has no gpg, so disable it.
  git("config commit.gpgsign false", fixture);

  // Commit a tracked file and an ignore rule first.
  writeFileSync(join(fixture, "tracked.txt"), "tracked\n");
  writeFileSync(join(fixture, ".gitignore"), "ignored.log\n");
  git("add -A", fixture);
  git('commit -m "init"', fixture);

  // Now create files that are genuinely untracked / ignored (after the commit).
  writeFileSync(join(fixture, "ignored.log"), "ignore me\n");
  writeFileSync(join(fixture, "untracked.txt"), "untracked\n");
  // An untracked, nested directory (uniform status, so it can collapse).
  writeFileWithDirs(join(fixture, "nested", "deep.txt"), "deep\n");

  repo = Repository.open(fixture);
});

afterAll(() => {
  rmSync(fixture, { recursive: true, force: true });
});

describe("Repository dirwalk (src/dirwalk.rs)", () => {
  test("default walk emits only untracked entries", async () => {
    const result = await repo.dirwalk([]);
    const paths = result.entries.map((e) => e.path).sort();
    // The ignored and tracked files are omitted by default.
    expect(paths).not.toContain("tracked.txt");
    expect(paths).not.toContain("ignored.log");
    // Untracked files are present (in default `Matching` mode, untracked directories
    // expand to their individual files rather than being collapsed).
    expect(paths).toContain("untracked.txt");
    expect(paths).toContain("nested/deep.txt");
    for (const e of result.entries) {
      expect(e.status).toBe("Untracked");
    }
  });

  test("the walk outcome carries the traversal root and counters", async () => {
    const result = await repo.dirwalk([]);
    // The traversal root is within the working directory and the counters are consistent.
    expect(result.traversalRoot).toBeTruthy();
    expect(result.readDirCalls).toBeGreaterThan(0);
    expect(result.seenEntries).toBeGreaterThanOrEqual(result.entries.length);
    expect(result.returnedEntries).toBe(result.entries.length);
    // Paths are relative to the traversal root, so joining must produce real paths.
    for (const e of result.entries) {
      expect(e.path).not.toMatch(/^\//);
    }
  });

  test("emitIgnored surfaces ignored entries", async () => {
    const result = await repo.dirwalk([], { emitIgnored: true });
    const ignored = result.entries.filter((e) => e.status.startsWith("Ignored"));
    expect(ignored.some((e) => e.path === "ignored.log")).toBe(true);
  });

  test("emitTracked surfaces tracked entries", async () => {
    const result = await repo.dirwalk([], { emitTracked: true });
    expect(result.entries.some((e) => e.path === "tracked.txt" && e.status === "Tracked")).toBe(true);
  });

  test("emitUntrackedCollapsed collapses the nested directory", async () => {
    const result = await repo.dirwalk([], { emitUntrackedCollapsed: true });
    // The nested directory itself is emitted as a single collapsed entry...
    expect(result.entries.some((e) => e.path === "nested")).toBe(true);
    // ...and its contents are not listed individually.
    expect(result.entries.some((e) => e.path === join("nested", "deep.txt"))).toBe(false);
  });

  test("emitCollapsed keeps entries inside collapsed directories observable", async () => {
    // `emitCollapsed` uses the `All` mode: every entry inside an about-to-be-collapsed
    // directory is emitted alongside the collapsed directory itself.
    const result = await repo.dirwalk([], { emitUntrackedCollapsed: true, emitCollapsed: true });
    expect(result.entries.some((e) => e.path === "nested")).toBe(true);
    expect(result.entries.some((e) => e.path === join("nested", "deep.txt"))).toBe(true);
    // Inner entries report the status of the collapsed directory they belong to.
    const inner = result.entries.find((e) => e.path === join("nested", "deep.txt"));
    expect(inner!.collapsedDirectoryStatus).toBe("Untracked");
  });

  test("forDeletion still walks and reports collapsed untracked directories", async () => {
    const result = await repo.dirwalk([], { forDeletion: true, emitUntrackedCollapsed: true });
    // Deletion mode must never collapse away entries that would be destroyed.
    expect(result.entries.some((e) => e.path === "nested")).toBe(true);
  });

  test("patterns limit the walk to matching paths", async () => {
    const result = await repo.dirwalk(["untracked.txt"]);
    expect(result.entries.map((e) => e.path)).toEqual(["untracked.txt"]);
  });
});
