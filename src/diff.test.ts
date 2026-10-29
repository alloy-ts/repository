import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Algorithm, diff, Repository } from "./main.ts";

let base: string;
let repo: Repository;

beforeAll(() => {
  base = mkdtempSync(join(tmpdir(), "diff-api-"));
  const dir = join(base, "repo");
  repo = Repository.init(dir, false);
});

afterAll(() => {
  rmSync(base, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 200,
  });
});

// ---------------------------------------------------------------------------
// imara-diff line diff (Algorithm enum + standalone `diff` function)
// ---------------------------------------------------------------------------

describe("imara line diff (Algorithm / diff)", () => {
  test("diff detects a single changed line (default Histogram)", () => {
    const hunks = diff("a\nb\nc\n", "a\nB\nc\n");
    expect(hunks.length).toBe(1);
    const h = hunks[0]!;
    expect(h.beforeStart).toBe(1);
    expect(h.beforeEnd).toBe(2);
    expect(h.afterStart).toBe(1);
    expect(h.afterEnd).toBe(2);
  });

  test("diff returns no hunks for identical input", () => {
    expect(diff("a\nb\nc\n", "a\nb\nc\n")).toEqual([]);
  });

  test("diff reports a line appended at the end", () => {
    const hunks = diff("a\nb\n", "a\nb\nc\n");
    expect(hunks.length).toBe(1);
    const h = hunks[0]!;
    expect(h.beforeStart).toBe(2);
    expect(h.beforeEnd).toBe(2);
    expect(h.afterStart).toBe(2);
    expect(h.afterEnd).toBe(3);
  });

  test("diff reports a removed line", () => {
    const hunks = diff("a\nb\nc\n", "a\nc\n");
    expect(hunks.length).toBe(1);
    const h = hunks[0]!;
    expect(h.beforeStart).toBe(1);
    expect(h.beforeEnd).toBe(2);
    expect(h.afterStart).toBe(1);
    expect(h.afterEnd).toBe(1);
  });

  test("diff reports two disjoint changes as separate hunks", () => {
    const hunks = diff("a\nb\nc\nd\ne\n", "a\nB\nc\nd\nE\n");
    expect(hunks.length).toBe(2);
    expect(hunks[0]!.beforeStart).toBe(1);
    expect(hunks[1]!.beforeStart).toBe(4);
  });

  test("Myers and Histogram agree for a simple change", () => {
    const withMyers = diff("a\nb\nc\n", "a\nB\nc\n", Algorithm.Myers);
    const withHistogram = diff("a\nb\nc\n", "a\nB\nc\n", Algorithm.Histogram);
    expect(withMyers).toEqual(withHistogram);
  });

  test("MyersMinimal produces the same hunk region as Myers here", () => {
    const minimal = diff("a\nb\nc\n", "a\nB\nc\n", Algorithm.MyersMinimal);
    expect(minimal).toEqual(diff("a\nb\nc\n", "a\nB\nc\n", Algorithm.Myers));
  });

  test("Algorithm enum exposes the three variants", () => {
    expect(Algorithm.Histogram).toBeDefined();
    expect(Algorithm.Myers).toBeDefined();
    expect(Algorithm.MyersMinimal).toBeDefined();
  });
});

// ---------------------------------------------------------------------------
// Repository.diffTreeToTree (tree-vs-tree diff, moved from repository.rs)
// ---------------------------------------------------------------------------

describe("Repository.diffTreeToTree", () => {
  function writeTree(content: string): string {
    const blob = repo.writeBlob(Buffer.from(content));
    return repo.writeTree([{ path: "file.txt", id: blob, mode: "100644" }]);
  }

  test("diffing a tree against itself yields no changes", () => {
    const tree = writeTree("hello\n");
    expect(repo.diffTreeToTree(tree, tree)).toEqual([]);
  });

  test("diffing empty against a tree reports all entries as Added", () => {
    const tree = writeTree("hello\n");
    const changes = repo.diffTreeToTree(undefined, tree);
    expect(changes.length).toBeGreaterThan(0);
    for (const c of changes) {
      expect(c.kind).toBe("Added");
      expect(c.oldId).toBeUndefined();
      expect(typeof c.newId).toBe("string");
    }
  });

  test("diffing two trees reports a modification with both ids", () => {
    const t1 = writeTree("hello\n");
    const t2 = writeTree("hello world\n");
    const changes = repo.diffTreeToTree(t1, t2);
    expect(changes.length).toBe(1);
    const c = changes[0]!;
    expect(c.kind).toBe("Modified");
    expect(typeof c.oldId).toBe("string");
    expect(typeof c.newId).toBe("string");
    expect(c.oldId).not.toBe(c.newId);
  });

  test("diffing a tree against an empty tree reports deletions", () => {
    const tree = writeTree("hello\n");
    const changes = repo.diffTreeToTree(tree, undefined);
    expect(changes.length).toBeGreaterThan(0);
    for (const c of changes) {
      expect(c.kind).toBe("Deleted");
      expect(typeof c.oldId).toBe("string");
      expect(c.newId).toBeUndefined();
    }
  });

  test("rename detection maps a move to a Renamed change with source location", () => {
    const blob = repo.writeBlob(Buffer.from("same content\n"));
    const t1 = repo.writeTree([{ path: "a.txt", id: blob, mode: "100644" }]);
    const t2 = repo.writeTree([{ path: "b.txt", id: blob, mode: "100644" }]);

    const renamed = repo.diffTreeToTree(t1, t2, {
      trackRenames: "Custom",
      renameThreshold: 50,
    });
    expect(renamed.length).toBe(1);
    expect(renamed[0]!.kind).toBe("Renamed");
    expect(renamed[0]!.sourceLocation).toBe("a.txt");
    expect(renamed[0]!.path).toBe("b.txt");

    // Disabled rename tracking reports the raw delete+add pair instead.
    const split = repo.diffTreeToTree(t1, t2, { trackRenames: "Disabled" });
    expect(split.map((c) => c.kind).sort()).toEqual(["Added", "Deleted"]);
  });

  test("sync and async diffTreeToTree agree", async () => {
    const t1 = writeTree("hello\n");
    const t2 = writeTree("hello world\n");
    const syncChanges = repo.diffTreeToTree(t1, t2);
    const asyncChanges = await repo.diffTreeToTreeAsync(t1, t2);
    expect(asyncChanges).toEqual(syncChanges);
  });
});

// ---------------------------------------------------------------------------
// Repository.diffTreeToIndex (tree-vs-index diff via gix status)
// ---------------------------------------------------------------------------

describe("Repository.diffTreeToIndex", () => {
  function writeTree2(files: { path: string; content: string }[]): string {
    const entries = files.map((f) => ({
      path: f.path,
      id: repo.writeBlob(Buffer.from(f.content)),
      mode: "100644",
    }));
    return repo.writeTree(entries);
  }

  test("a tree entry missing from the (empty) index is reported as Deleted", () => {
    const tree = writeTree2([{ path: "only.txt", content: "x\n" }]);
    const changes = repo.diffTreeToIndex(tree);
    expect(changes.length).toBe(1);
    expect(changes[0]!.kind).toBe("Deleted");
    expect(changes[0]!.path).toBe("only.txt");
    expect(typeof changes[0]!.oldId).toBe("string");
    expect(changes[0]!.newId).toBeUndefined();
  });

  test("pathspec limits which entries are compared", () => {
    const tree = writeTree2([
      { path: "keep.txt", content: "k\n" },
      { path: "skip.txt", content: "s\n" },
    ]);
    const changes = repo.diffTreeToIndex(tree, { pathspec: ["keep.txt"] });
    expect(changes.length).toBe(1);
    expect(changes[0]!.path).toBe("keep.txt");
  });

  test("sync and async diffTreeToIndex agree", async () => {
    const tree = writeTree2([{ path: "f.txt", content: "v\n" }]);
    const syncChanges = repo.diffTreeToIndex(tree);
    const asyncChanges = await repo.diffTreeToIndexAsync(tree);
    expect(asyncChanges).toEqual(syncChanges);
  });
});

// ---------------------------------------------------------------------------
// Repository.diffIndexToWorkdir / diffTreeToWorkdir (worktree diffs)
// ---------------------------------------------------------------------------

describe("Repository.diffIndexToWorkdir", () => {
  test("an untracked working-tree file is reported as Added", () => {
    writeFileSync(join(base, "repo", "untracked.txt"), "u\n");
    const changes = repo.diffIndexToWorkdir();
    const untracked = changes.filter((c) => c.path === "untracked.txt");
    expect(untracked.length).toBe(1);
    expect(untracked[0]!.kind).toBe("Added");
    expect(untracked[0]!.entryMode).toBe("100644");
  });

  test("untracked: 'none' omits untracked files", () => {
    const changes = repo.diffIndexToWorkdir({ untracked: "None" });
    expect(changes.filter((c) => c.path === "untracked.txt")).toEqual([]);
  });

  test("sync and async diffIndexToWorkdir agree", async () => {
    const syncChanges = repo.diffIndexToWorkdir();
    const asyncChanges = await repo.diffIndexToWorkdirAsync();
    expect(asyncChanges).toEqual(syncChanges);
  });
});

describe("Repository.diffTreeToWorkdir", () => {
  test("combines the tree-to-index and index-to-worktree halves", () => {
    // Tree contains tracked.txt; the index is empty and the worktree has untracked.txt.
    const blob = repo.writeBlob(Buffer.from("tracked\n"));
    const tree = repo.writeTree([
      {
        path: "tracked.txt",
        id: blob,
        mode: "100644",
      },
    ]);
    const changes = repo.diffTreeToWorkdir(tree);

    const deleted = changes.find((c) => c.path === "tracked.txt");
    expect(deleted?.kind).toBe("Deleted");
    const added = changes.find((c) => c.path === "untracked.txt");
    expect(added?.kind).toBe("Added");
  });

  test("sync and async diffTreeToWorkdir agree", async () => {
    const blob = repo.writeBlob(Buffer.from("tracked2\n"));
    const tree = repo.writeTree([
      {
        path: "tracked2.txt",
        id: blob,
        mode: "100644",
      },
    ]);
    const syncChanges = repo.diffTreeToWorkdir(tree);
    const asyncChanges = await repo.diffTreeToWorkdirAsync(tree);
    expect(asyncChanges).toEqual(syncChanges);
  });
});
