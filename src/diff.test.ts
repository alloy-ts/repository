import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { mkdtempSync, rmSync } from "node:fs";
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
    const h = hunks[0];
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
    const h = hunks[0];
    expect(h.beforeStart).toBe(2);
    expect(h.beforeEnd).toBe(2);
    expect(h.afterStart).toBe(2);
    expect(h.afterEnd).toBe(3);
  });

  test("diff reports a removed line", () => {
    const hunks = diff("a\nb\nc\n", "a\nc\n");
    expect(hunks.length).toBe(1);
    const h = hunks[0];
    expect(h.beforeStart).toBe(1);
    expect(h.beforeEnd).toBe(2);
    expect(h.afterStart).toBe(1);
    expect(h.afterEnd).toBe(1);
  });

  test("diff reports two disjoint changes as separate hunks", () => {
    const hunks = diff("a\nb\nc\nd\ne\n", "a\nB\nc\nd\nE\n");
    expect(hunks.length).toBe(2);
    expect(hunks[0].beforeStart).toBe(1);
    expect(hunks[1].beforeStart).toBe(4);
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
      expect(c.oldId).toBe(null);
      expect(typeof c.newId).toBe("string");
    }
  });

  test("diffing two trees reports a modification with both ids", () => {
    const t1 = writeTree("hello\n");
    const t2 = writeTree("hello world\n");
    const changes = repo.diffTreeToTree(t1, t2);
    expect(changes.length).toBe(1);
    const c = changes[0];
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
      expect(c.newId).toBe(null);
    }
  });
});
