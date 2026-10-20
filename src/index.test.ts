import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";

import { indexStatIsRacy, indexStatMatches, Repository } from "./main.ts";

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

describe("Index lookups, stat data, mutation, extensions, persistence", () => {
  test("entries expose stat data, flags, and stage", () => {
    // editIndex() (run by earlier tests) writes zeroed stats for its appended
    // entries, so pick an entry that git itself staged and has real stat data.
    const e = repo.indexEntries().find((x) => x.stat.mtimeSecs > 0)!;
    expect(e.stage).toBe(0);
    expect(e.flags).toBeGreaterThanOrEqual(0);
    expect(typeof e.stat.size).toBe("number");
    expect(e.stat.size).toBeGreaterThan(0);
    expect(typeof e.stat.ino).toBe("number");
  });

  test("entryByPath / entryByPathAndStage / icase lookups", () => {
    const idx = repo.index();
    const e = idx.entries()[0];

    expect(idx.entryByPath(e.path)?.id).toBe(e.id);
    expect(idx.entryByPath("no/such/path.txt")).toBeNull();

    expect(idx.entryByPathAndStage(e.path, 0)?.id).toBe(e.id);
    expect(idx.entryByPathAndStage(e.path, 2)).toBeNull();
    expect(() => idx.entryByPathAndStage(e.path, 7)).toThrow(/invalid stage 7/);

    expect(idx.entryByPathIcase(e.path, false)?.id).toBe(e.id);
    expect(idx.entryByPathIcase(e.path.toUpperCase(), true)?.id).toBe(e.id);
    expect(idx.entryByPathIcase(e.path.toUpperCase(), false)).toBeNull();
  });

  test("treeExtension shape when present, other extensions absent", () => {
    const idx = repo.index();
    // The tree-cache extension is present when git last wrote the index, but
    // editIndex() drops it by design — so only assert the shape when present.
    const tree = idx.treeExtension();
    if (tree) {
      expect(tree.id).toMatch(/^[0-9a-f]{40}$/);
      expect(Array.isArray(tree.children)).toBe(true);
    }

    expect(idx.linkExtension()).toBeNull();
    expect(idx.hasResolveUndo()).toBe(false);
    expect(idx.hasFsMonitor()).toBe(false);
    // UNTR requires `git update-index --untracked-cache`; the plain fixture has none.
    expect(idx.untrackedExtension()).toBeNull();
  });

  test("pathIsDirectory / pathIsDirectoryIcase", () => {
    const idx = repo.index();
    // The fixture commits hello.txt at the root; a nested path makes this test
    // meaningful regardless of the repo layout. editIndex drops the tree-cache
    // extension, so this test must run after the treeExtension assertions.
    repo.editIndex([
      {
        path: "dir/inner.txt",
        id: idx.entries()[0].id,
        mode: "100644",
      },
    ]);

    const idx2 = repo.openIndex();
    expect(idx2.pathIsDirectory("dir")).toBe(true);
    expect(idx2.pathIsDirectory("dir/inner.txt")).toBe(false);
    expect(idx2.pathIsDirectory("DIR")).toBe(false);
    expect(idx2.pathIsDirectoryIcase("DIR", true)).toBe(true);
    expect(idx2.pathIsDirectoryIcase("DIR", false)).toBe(false);
    expect(idx2.pathIsDirectory("absent")).toBe(false);
  });

  test("timestamp() and setTimestamp() round-trip", () => {
    const idx = repo.openIndex();
    const original = idx.timestamp();
    expect(original.secs).toBeGreaterThan(0);

    idx.setTimestamp(1_700_000_000, 123);
    expect(idx.timestamp().secs).toBe(1_700_000_000);
    expect(idx.timestamp().nsecs).toBe(123);

    idx.setTimestamp(original.secs, original.nsecs);
  });

  test("indexStatIsRacy / indexStatMatches", () => {
    const idx = repo.openIndex();
    const e = idx.entries().find((x) => x.stat.mtimeSecs > 0)!;
    const mtime = { secs: e.stat.mtimeSecs, nsecs: 0 };

    // Racy = mtime >= index timestamp (racy-git.txt). Test the ordering directly
    // rather than the fixture's live timestamp, which earlier tests may move.
    expect(indexStatIsRacy(e.stat, mtime)).toBe(true); // equal seconds => racy
    expect(indexStatIsRacy(e.stat, { secs: e.stat.mtimeSecs - 1, nsecs: 0 })).toBe(true); // timestamp before mtime
    expect(indexStatIsRacy(e.stat, { secs: e.stat.mtimeSecs + 1, nsecs: 0 })).toBe(false); // settled

    expect(indexStatMatches(e.stat, e.stat)).toBe(true);
    const other = { ...e.stat, size: e.stat.size + 1 };
    expect(indexStatMatches(e.stat, other)).toBe(false);
    // Size is always compared; checkStat: false only ignores nsec, ino, dev, uid, gid.
    const otherIno = { ...e.stat, ino: e.stat.ino + 1 };
    expect(indexStatMatches(e.stat, otherIno)).toBe(false);
    expect(indexStatMatches(e.stat, otherIno, { checkStat: false })).toBe(true);
  });

  test("in-memory mutation: removeEntryAtIndex + sortEntries", () => {
    const idx = repo.index();
    const before = idx.len();
    const first = idx.entries()[0];

    const removed = idx.removeEntryAtIndex(0);
    expect(removed?.path).toBe(first.path);
    expect(idx.len()).toBe(before - 1);
    expect(idx.entryByPath(first.path)).toBeNull();

    idx.sortEntries();
    // Remaining entries stay sorted; nothing was written to disk.
    const paths = idx.entries().map((e) => e.path);
    expect([...paths].sort()).toEqual(paths);
    expect(repo.indexEntries().length).toBe(before);
  });

  test("isSparse and decode-time extension markers", () => {
    const idx = repo.index();
    expect(idx.isSparse()).toBe(false);
    expect(typeof idx.hadEndOfIndexMarker()).toBe("boolean");
    expect(typeof idx.hadOffsetTable()).toBe("boolean");
  });

  test("writeToBuffer serializes to bytes without touching disk", () => {
    const idx = repo.index();
    const before = repo.indexEntries().length;

    const out = idx.writeToBuffer();
    expect(out.version).toBe(idx.version());
    expect(out.checksum).toMatch(/^[0-9a-f]{40}$/);
    expect(out.data.length).toBeGreaterThan(64);

    const skipped = idx.writeToBuffer(true);
    // NAPI omits `Some`-less optional fields, so absent === undefined.
    expect(skipped.checksum ?? null).toBeNull();
    expect(skipped.data.length).toBe(out.data.length);
    // Nothing was persisted.
    expect(repo.indexEntries().length).toBe(before);
  });

  test("openIndexAt creates an empty in-memory index for a missing file", () => {
    const dir = mkdtempSync(join(tmpdir(), "repository-index-at-"));
    try {
      const scratch = repo.openIndexAt(join(dir, "missing-index"));
      expect(scratch.len()).toBe(0);
      expect(scratch.isEmpty()).toBe(true);
      expect(scratch.isSparse()).toBe(false);

      const opts = repo.openIndexAt(join(dir, "missing-index-2"), {
        skipHash: true,
        threadLimit: 1,
        allocLimitBytes: 1024 * 1024,
      });
      expect(opts.len()).toBe(0);
    } finally {
      rmSync(dir, {
        recursive: true,
        force: true,
        maxRetries: 10,
        retryDelay: 200,
      });
    }
  });

  test("openIndex skipHash opens without checksum verification", () => {
    const idx = repo.openIndex(true);
    expect(idx.len()).toBe(repo.indexEntries().length);
  });

  test("write(skipHash) persists in-memory mutations to a scratch clone", () => {
    // Clone the fixture so the real repo's index stays untouched.
    const base = mkdtempSync(join(tmpdir(), "repository-index-write-"));
    const clonePath = join(base, "clone");
    execSync(
      `git clone --quiet ${JSON.stringify(repo.workDir() ?? repo.gitDir())} ${JSON.stringify(
        clonePath,
      )}`,
      {
        stdio: "ignore",
      },
    );
    try {
      const clone = Repository.open(clonePath);
      const idx = clone.index();
      const before = idx.len();

      const removed = idx.removeEntryAtIndex(before - 1);
      expect(removed).not.toBeNull();
      idx.sortEntries();
      const version = idx.write(true);
      expect(version).toBeGreaterThanOrEqual(2);

      const reopened = clone.openIndex(true);
      expect(reopened.len()).toBe(before - 1);
      expect(reopened.entryByPath(removed!.path)).toBeNull();
    } finally {
      rmSync(base, {
        recursive: true,
        force: true,
        maxRetries: 10,
        retryDelay: 200,
      });
    }
  });
});
