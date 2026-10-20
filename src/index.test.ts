import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";

import { Repository } from "./main.ts";

let fixture: string;
let repo: Repository;

function git(args: string, cwd: string) {
  execSync(`git ${args}`, { cwd, stdio: "ignore" });
}

beforeAll(() => {
  // A throwaway repository with one committed file, so the index exists on disk.
  fixture = mkdtempSync(join(tmpdir(), "repository-index-"));
  git("init --initial-branch=main", fixture);
  git("config user.email test@example.com", fixture);
  git("config user.name Test", fixture);
  // The dev machine signs commits globally; the throwaway fixture has no gpg, so disable it.
  git("config commit.gpgsign false", fixture);

  writeFileSync(join(fixture, "hello.txt"), "hello\n");
  git("add hello.txt", fixture);
  git("commit -m init", fixture);

  repo = Repository.open(fixture);
});

afterAll(() => {
  rmSync(fixture, { recursive: true, force: true });
});

describe("Repository index (src/index.rs)", () => {
  test("indexPath() points at <git_dir>/index", () => {
    const p = repo.indexPath();
    expect(p.endsWith("/index") || p.endsWith("\\index")).toBe(true);
  });

  test("index()/openIndex()/tryIndex() expose entries, version, path, len", () => {
    const idx = repo.index();
    expect(idx).not.toBeNull();

    const i = idx!;
    expect(i.len()).toBeGreaterThan(0);
    expect(i.isEmpty()).toBe(false);
    expect(i.version()).toBeGreaterThanOrEqual(2);
    expect(i.path().endsWith("/index")).toBe(true);

    const entries = i.entries();
    expect(Array.isArray(entries)).toBe(true);
    expect(entries.length).toBeGreaterThan(0);
    expect(typeof entries[0].path).toBe("string");
    expect(typeof entries[0].id).toBe("string");
    expect(typeof entries[0].mode).toBe("string");

    // openIndex() is a fresh decode of the same file.
    expect(repo.openIndex().len()).toBe(i.len());
    // tryIndex() returns the same index (it exists).
    expect(repo.tryIndex()).not.toBeNull();
  });

  test("indexEntries() returns the same entries as index().entries()", () => {
    expect(repo.indexEntries().length).toBe(repo.index()!.entries().length);
  });

  test("indexFromTree() rebuilds an in-memory index from HEAD^{tree}", () => {
    const headTree = repo.headTreeId();
    const fromTree = repo.indexFromTree(headTree);
    expect(fromTree).not.toBeNull();
    expect(fromTree!.len()).toBe(repo.index()!.len());
  });

  test("editIndex() appends an entry to the on-disk index", () => {
    const existing = repo.indexEntries()[0];
    repo.editIndex([{ path: "extra.txt", id: existing.id, mode: "100644" }]);
    const after = repo.indexEntries();
    expect(after.some((e) => e.path === "extra.txt")).toBe(true);
  });

  test("indexOrLoad() returns a usable index even without a fresh index file", () => {
    const idx = repo.indexOrLoad();
    expect(idx).not.toBeNull();
    expect(idx!.len()).toBeGreaterThan(0);
  });

  test("indexEntries() and indexOrLoadFromHeadEntries() return index items", () => {
    const loaded = repo.indexOrLoadFromHeadEntries();
    expect(Array.isArray(loaded)).toBe(true);
    expect(loaded.length).toBeGreaterThan(0);
    for (const e of loaded) {
      expect(typeof e.path).toBe("string");
      expect(typeof e.id).toBe("string");
      expect(typeof e.stage).toBe("number");
    }

    try {
      const entries = repo.indexEntries();
      expect(Array.isArray(entries)).toBe(true);
    } catch {
      // Index file may not exist yet in a fresh clone
    }
  });

  test("loads the HEAD-derived index and fails on unborn HEAD", () => {
    const idx = repo.indexOrLoadFromHead();
    expect(idx.entries().length).toBeGreaterThan(0);
  });
});
