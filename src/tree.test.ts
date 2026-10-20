import { execSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-tree-"));
  cloneDir = join(base, "clone");
  try {
    repo = await Repository.clone(realRoot, cloneDir, false);
  } catch {
    rmSync(cloneDir, { recursive: true, force: true });
    execSync(`git clone --local --quiet ${JSON.stringify(realRoot)} ${JSON.stringify(cloneDir)}`, {
      stdio: "ignore",
    });
    repo = Repository.open(cloneDir);
  }
});

afterAll(() => {
  rmSync(base, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 200,
  });
});

function makeEntryTree(): { treeId: string; blobId: string } {
  const blobId = repo.writeBlob(Buffer.from("tree fixture content\n"));
  const treeId = repo.writeTree([
    {
      path: "file.txt",
      mode: "100644",
      id: blobId,
    },
  ]);
  return { treeId, blobId };
}

describe("Repository trees", () => {
  test("findTree resolves the commit's tree", () => {
    const tree = repo.findTree(repo.headCommit().treeId);
    expect(tree.id).toBe(repo.headCommit().treeId);
    expect(Array.isArray(tree.entries)).toBe(true);
  });

  test("entries expose mode, filename, and id", () => {
    const { treeId, blobId } = makeEntryTree();
    const entries = repo.findTree(treeId).entries;
    expect(entries.length).toBe(1);
    expect(entries[0].mode).toBe("100644");
    expect(entries[0].filename).toBe("file.txt");
    expect(entries[0].id).toBe(blobId);
  });

  test("entry() looks up an entry by filename", () => {
    const { treeId, blobId } = makeEntryTree();
    const tree = repo.findTree(treeId);
    expect(tree.entry("file.txt")?.id).toBe(blobId);
    expect(tree.entry("file.txt")?.filename).toBe("file.txt");
    expect(tree.entry("missing.txt")).toBeNull();
  });

  test("entryByPath() resolves nested paths", () => {
    const blobId = repo.writeBlob(Buffer.from("nested path\n"));
    const treeId = repo.editTree(repo.emptyTree().id, [
      {
        path: "a/b/c.txt",
        id: blobId,
      },
    ]);
    const tree = repo.findTree(treeId);

    const entry = tree.entryByPath("a/b/c.txt");
    expect(entry?.id).toBe(blobId);
    expect(entry?.filename).toBe("c.txt");
    expect(entry?.mode).toBe("100644");
    expect(entry?.treeId).toBe(treeId);
    expect(entry?.isTree()).toBe(false);
    expect(entry?.asTree()).toBeNull();

    const dir = tree.entryByPath("a/b");
    expect(dir).not.toBeNull();
    expect(dir?.isTree()).toBe(true);
    expect(dir?.filename).toBe("b");
    const subtree = dir?.asTree();
    expect(subtree).not.toBeNull();
    expect(subtree?.entry("c.txt")?.id).toBe(blobId);

    expect(tree.entryByPath("a/b/missing.txt")).toBeNull();
    expect(tree.entryByPath("a/missing/b.txt")).toBeNull();
    expect(tree.entryByPath("missing/b/c.txt")).toBeNull();
    expect(tree.entryByPath("")).toBeNull();
  });

  test("writeTree() creates a tree and tryFindObject() retrieves it", () => {
    const { treeId } = makeEntryTree();
    expect(treeId).toMatch(/^[0-9a-f]{40}$/);

    const found = repo.tryFindObject(treeId);
    expect(found).not.toBeNull();
    expect(found?.kind).toBe("tree");

    const missing = repo.tryFindObject("0".repeat(40));
    expect(missing).toBeNull();
  });

  test("writeTree() rejects an invalid mode", () => {
    const blobId = repo.writeBlob(Buffer.from("bad mode\n"));
    expect(() => repo.writeTree([{ path: "file.txt", mode: "999999", id: blobId }])).toThrow(
      /invalid file mode 999999/,
    );
  });

  test("editTree() upserts an entry on top of the empty tree", () => {
    const blobId = repo.writeBlob(Buffer.from("x"));
    const treeId = repo.editTree(repo.emptyTree().id, [
      { path: "x.txt", mode: "100644", id: blobId },
    ]);
    expect(treeId).toMatch(/^[0-9a-f]{40}$/);
    const tree = repo.findTree(treeId);
    expect(tree.entries.length).toBe(1);
    expect(tree.entries[0].filename).toBe("x.txt");
  });

  test("editTree() creates intermediate trees for nested paths", () => {
    const blobId = repo.writeBlob(Buffer.from("nested\n"));
    const treeId = repo.editTree(repo.emptyTree().id, [
      {
        path: "a/b/c.txt",
        id: blobId,
      },
    ]);
    const tree = repo.findTree(treeId);
    expect(tree.entries.length).toBe(1);
    expect(tree.entries[0].mode).toBe("040000");
    expect(tree.entries[0].filename).toBe("a");

    const sub = repo.findTree(tree.entries[0].id);
    expect(sub.entries[0].filename).toBe("b");
    const leaf = repo.findTree(sub.entries[0].id);
    expect(leaf.entries[0].filename).toBe("c.txt");
    expect(leaf.entries[0].id).toBe(blobId);
  });

  test("editTree() removes entries and tolerates missing paths", () => {
    const { treeId } = makeEntryTree();
    const removed = repo.editTree(treeId, [{ path: "file.txt", remove: true }]);
    expect(repo.findTree(removed).entries.length).toBe(0);

    const noop = repo.editTree(treeId, [
      {
        path: "not-there.txt",
        remove: true,
      },
    ]);
    expect(repo.findTree(noop).entries.length).toBe(1);
  });

  test("editTree() removeMode leaf rejects trees, ifLeaf skips trees", () => {
    const blobId = repo.writeBlob(Buffer.from("leaf modes\n"));
    const treeId = repo.editTree(repo.emptyTree().id, [
      {
        path: "d/x.txt",
        id: blobId,
      },
    ]);

    // "leaf" errors when the target path is a tree.
    expect(() => repo.editTree(treeId, [{ path: "d", remove: true, removeMode: "leaf" }])).toThrow(
      /is a tree/,
    );
    // ... but works on the leaf itself.
    const removedLeaf = repo.editTree(treeId, [
      { path: "d/x.txt", remove: true, removeMode: "leaf" },
    ]);
    expect(repo.findTree(removedLeaf).entries.length).toBe(0);
  });

  test("editTree() removeMode ifLeaf leaves trees untouched", () => {
    const blobId = repo.writeBlob(Buffer.from("ifleaf\n"));
    const otherId = repo.writeBlob(Buffer.from("sibling\n"));
    const treeId = repo.editTree(repo.emptyTree().id, [
      { path: "d/x.txt", id: blobId },
      { path: "d/y.txt", id: otherId },
    ]);

    // "ifLeaf" is a no-op when the target is a tree.
    const untouched = repo.editTree(treeId, [
      {
        path: "d",
        remove: true,
        removeMode: "ifLeaf",
      },
    ]);
    expect(repo.findTree(untouched).entryByPath("d/x.txt")?.id).toBe(blobId);
    expect(repo.findTree(untouched).entryByPath("d/y.txt")?.id).toBe(otherId);

    // ... and removes actual leaves. The tree `d` survives while it still has entries.
    const removed = repo.editTree(treeId, [
      { path: "d/x.txt", remove: true, removeMode: "ifLeaf" },
    ]);
    expect(repo.findTree(removed).entryByPath("d")).not.toBeNull();
    expect(repo.findTree(removed).entryByPath("d/x.txt")).toBeNull();
    expect(repo.findTree(removed).entryByPath("d/y.txt")?.id).toBe(otherId);

    // Removing the last entry prunes the now-empty tree, like git does.
    const pruned = repo.editTree(removed, [
      { path: "d/y.txt", remove: true, removeMode: "ifLeaf" },
    ]);
    expect(repo.findTree(pruned).entryByPath("d")).toBeNull();
  });

  test("editTree() rejects an invalid removeMode", () => {
    const { treeId } = makeEntryTree();
    expect(() =>
      repo.editTree(treeId, [
        {
          path: "file.txt",
          remove: true,
          removeMode: "bogus",
        },
      ]),
    ).toThrow(/invalid removeMode bogus/);
  });

  test("emptyTree() returns the well-known id", () => {
    expect(repo.emptyTree().id).toBe("4b825dc642cb6eb9a060e54bf8d69288fbee4904");
  });

  test("diffTreeToTree() diffs two trees and against empty", () => {
    const head = repo.findCommit(repo.headId());
    const changes = repo.diffTreeToTree(null, head.treeId);
    expect(Array.isArray(changes)).toBe(true);
    expect(changes.length).toBeGreaterThan(0);
    for (const ch of changes) {
      expect(typeof ch.path).toBe("string");
      expect(typeof ch.kind).toBe("string");
    }

    if (repo.log().length >= 2) {
      const prev = repo.findCommit(repo.revParseSingle("HEAD~1"));
      const twoTree = repo.diffTreeToTree(prev.treeId, head.treeId);
      expect(Array.isArray(twoTree)).toBe(true);
    }
  });
});
