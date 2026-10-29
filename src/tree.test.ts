import { execSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository, Tree, TreeBuilder } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-tree-"));
  cloneDir = join(base, "clone");
  try {
    repo = await Repository.clone(realRoot, cloneDir);
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

function makeNestedTree(): {
  treeId: string;
  root: string;
  ids: Record<string, string>;
} {
  const fileId = repo.writeBlob(Buffer.from("file content\n"));
  const topId = repo.writeBlob(Buffer.from("top content\n"));
  const cId = repo.writeBlob(Buffer.from("nested c\n"));
  const dId = repo.writeBlob(Buffer.from("a d\n"));
  const builder = Tree.build(repo);
  builder.upsert("file.txt", fileId);
  builder.upsert("top.txt", topId);
  builder.upsert("a/b/c.txt", cId);
  builder.upsert("a/d.txt", dId);
  const tree = builder.write();
  return {
    treeId: tree.id,
    root: tree.canonicalise(),
    ids: {
      "file.txt": fileId,
      "top.txt": topId,
      "a/b/c.txt": cId,
      "a/d.txt": dId,
    },
  };
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

  test("len / isEmpty report the direct entry count", () => {
    const { treeId, blobId } = makeEntryTree();
    const tree = repo.findTree(treeId);
    expect(tree.len()).toBe(1);
    expect(tree.isEmpty).toBe(false);
    expect(repo.findTree(repo.emptyTree().id).isEmpty).toBe(true);
    // entries(), get(), and getById() all agree on the same entry.
    expect(tree.get(0)?.id).toBe(blobId);
    expect(tree.get(1)).toBeNull();
  });

  test("getById() looks up an entry by object id", () => {
    const { treeId, blobId } = makeEntryTree();
    const tree = repo.findTree(treeId);
    expect(tree.getById(blobId)?.filename).toBe("file.txt");
    expect(tree.getById("0".repeat(40))).toBeNull();
  });

  test("asObject() views the tree as a generic Object", () => {
    const { treeId } = makeEntryTree();
    const obj = repo.findTree(treeId).asObject();
    expect(obj.id).toBe(treeId);
    expect(obj.kind).toBe("tree");
  });

  test("TreeEntry exposes name, filemode, filemodeRaw, and toObject", () => {
    const { treeId, blobId } = makeEntryTree();
    const entry = repo.findTree(treeId).entries[0];
    expect(entry.name).toBe("file.txt"); // alias of `filename`
    expect(entry.filemode).toBe(0o100644);
    expect(entry.filemodeRaw).toBe(0o100644);
    const obj = entry.toObject(repo);
    expect(obj.kind).toBe("blob");
    expect(obj.id).toBe(blobId);
  });

  test("canonicalise() returns a 64-hex prolly root for a git tree", () => {
    const { treeId } = makeEntryTree();
    const root = repo.findTree(treeId).canonicalise();
    expect(root).toMatch(/^[0-9a-f]{64}$/);
    // Deterministic: identical content always yields an identical root.
    expect(repo.findTree(treeId).canonicalise()).toBe(root);
  });

  test("prollyTree(root) reconstructs the tree and its git id matches", () => {
    const { treeId } = makeEntryTree();
    const root = repo.findTree(treeId).canonicalise();
    const tree = repo.prollyTree(root);
    // The derived git oid equals the original tree (git stays the cache).
    expect(tree.id).toBe(treeId);
    // canonicalise short-circuits back to the same root.
    expect(tree.canonicalise()).toBe(root);
    expect(tree.entries[0].filename).toBe("file.txt");
    expect(tree.entryByPath("file.txt")?.id).toBe(
      repo.findTree(treeId).entryByPath("file.txt")?.id,
    );
  });

  test("prolly-backed reads (served from the flat map) match the git tree", () => {
    const { treeId, root, ids } = makeNestedTree();
    const gitTree = repo.findTree(treeId);
    const prollyTree = repo.prollyTree(root);

    // The derived git oid matches the original tree (git stays the cache).
    expect(prollyTree.id).toBe(treeId);

    // entries: same names, modes, and derived subtree oids.
    const gitEntries = gitTree.entries;
    const prollyEntries = prollyTree.entries;
    expect(prollyEntries.map((e) => e.filename).sort()).toEqual(
      gitEntries.map((e) => e.filename).sort(),
    );
    for (const name of ["file.txt", "top.txt", "a"]) {
      const g = gitEntries.find((e) => e.filename === name)!;
      const p = prollyEntries.find((e) => e.filename === name)!;
      expect(p.mode).toBe(g.mode);
      expect(p.id).toBe(g.id);
    }
    // Directory entries are advertised as trees with a derived git oid.
    const dir = prollyEntries.find((e) => e.filename === "a")!;
    expect(dir.mode).toBe("040000");
    expect(dir.id).toBe(gitEntries.find((e) => e.filename === "a")!.id);

    // len / get / getById / entry agree with the git-backed tree.
    expect(prollyTree.len()).toBe(gitTree.len());
    for (let i = 0; i < gitTree.len(); i++) {
      expect(prollyTree.get(i)?.id).toBe(gitTree.get(i)?.id);
      expect(prollyTree.get(i)?.filename).toBe(gitTree.get(i)?.filename);
    }
    for (const name of ["file.txt", "top.txt", "a"]) {
      expect(prollyTree.entry(name)?.id).toBe(gitTree.entry(name)?.id);
      expect(prollyTree.getById(gitTree.entry(name)!.id)?.filename).toBe(name);
    }

    // entryByPath resolves leaves and directories identically.
    for (const path of ["file.txt", "top.txt", "a", "a/b/c.txt", "a/d.txt"]) {
      const g = gitTree.entryByPath(path);
      const p = prollyTree.entryByPath(path);
      expect(p?.id).toBe(g?.id);
      expect(p?.isTree()).toBe(g?.isTree());
      expect(p?.filename).toBe(g?.filename);
    }
    // Leaf ids are the actual blobs (read straight off the flat map).
    expect(prollyTree.entryByPath("file.txt")?.id).toBe(ids["file.txt"]);
    expect(prollyTree.entryByPath("a/b/c.txt")?.id).toBe(ids["a/b/c.txt"]);

    // walk yields every leaf with matching path/mode/id.
    const key = (w: { path: string; mode: string; id: string }) => `${w.path}:${w.mode}:${w.id}`;
    expect(prollyTree.walk().map(key).sort()).toEqual(gitTree.walk().map(key).sort());
    // walk with contents still returns the blob bytes for leaves.
    const withContent = prollyTree.walk({ contents: true });
    const fileEntry = withContent.find((w) => w.path === "file.txt")!;
    expect(fileEntry.content).toBeInstanceOf(Buffer);
    expect(fileEntry.content!.toString()).toBe("file content\n");

    // intoEntries returns live Entry objects matching the git tree's.
    const gitInto = gitTree.intoEntries();
    const prollyInto = prollyTree.intoEntries();
    expect(prollyInto.map((e) => e.filename).sort()).toEqual(gitInto.map((e) => e.filename).sort());
    for (const name of ["file.txt", "top.txt", "a"]) {
      const g = gitInto.find((e) => e.filename === name)!;
      const p = prollyInto.find((e) => e.filename === name)!;
      expect(p.id).toBe(g.id);
      expect(p.isTree()).toBe(g.isTree());
    }
  });

  test("TreeBuilder.writeProlly returns the canonical prolly root", () => {
    const blobId = repo.writeBlob(Buffer.from("prolly write\n"));
    const builder = Tree.build(repo);
    builder.file("p.txt", blobId);
    const root = builder.writeProlly();
    expect(root).toMatch(/^[0-9a-f]{64}$/);
    // The git tree written alongside has the same canonical root.
    const gitTree = builder.write();
    expect(gitTree.canonicalise()).toBe(root);
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

describe("TreeBuilder", () => {
  test("Tree.build() builds on the empty tree, nested paths included", () => {
    const blobId = repo.writeBlob(Buffer.from("builder content\n"));
    const builder = Tree.build(repo);
    builder.upsert("file.txt", blobId);
    builder.upsert("a/b/c.txt", blobId);
    const tree = builder.write();

    expect(tree.entryByPath("file.txt")?.id).toBe(blobId);
    expect(tree.entryByPath("a/b/c.txt")?.id).toBe(blobId);
    expect(tree.entryByPath("a/b")?.isTree()).toBe(true);

    // Deterministic serialization: same edits via editTree produce the same id.
    const viaEditTree = repo.editTree(repo.emptyTree().id, [
      { path: "file.txt", id: blobId },
      { path: "a/b/c.txt", id: blobId },
    ]);
    expect(tree.id).toBe(viaEditTree);
  });

  test("new TreeBuilder(repo, baseTreeId) applies edits on top of a base tree", () => {
    const { treeId, blobId } = makeEntryTree();
    const replacement = repo.writeBlob(Buffer.from("replacement\n"));

    const builder = new TreeBuilder(repo, treeId);
    builder.upsert("file.txt", replacement);
    const tree = builder.write();
    expect(tree.entries.length).toBe(1);
    expect(tree.entries[0].id).toBe(replacement);

    const removed = new TreeBuilder(repo, treeId);
    removed.remove("file.txt");
    expect(removed.write().entries.length).toBe(0);
    expect(removed.write().id).toBe(repo.emptyTree().id);

    // Still resolvable through the base entry's id.
    expect(blobId).toMatch(/^[0-9a-f]{40}$/);
  });

  test("write() rejects a missing object by default; validate:false skips the check", () => {
    const missing = "1".repeat(40);
    const builder = Tree.build(repo);
    builder.upsert("file.txt", missing);
    expect(() => builder.write()).toThrow(/could not be found/);

    const unchecked = Tree.build(repo);
    unchecked.upsert("file.txt", missing);
    const tree = unchecked.write({ validate: false });
    expect(tree.entryByPath("file.txt")?.id).toBe(missing);
  });

  test("upsert() fails fast on an invalid mode or object id", () => {
    const blobId = repo.writeBlob(Buffer.from("fail fast\n"));
    expect(() => Tree.build(repo).upsert("f.txt", blobId, "999999")).toThrow(
      /invalid file mode 999999/,
    );
    expect(() => Tree.build(repo).upsert("f.txt", "nothex")).toThrow();
  });

  test("remove() rejects an invalid removeMode eagerly", () => {
    expect(() => Tree.build(repo).remove("f.txt", "bogus")).toThrow(/invalid removeMode bogus/);
  });

  test("removeMode leaf errors on trees, ifLeaf leaves trees untouched", () => {
    const blobId = repo.writeBlob(Buffer.from("leaf modes\n"));
    const base = repo.editTree(repo.emptyTree().id, [
      {
        path: "d/x.txt",
        id: blobId,
      },
    ]);

    const leaf = new TreeBuilder(repo, base);
    leaf.remove("d", "leaf");
    expect(() => leaf.write()).toThrow(/is a tree/);

    const ifLeaf = new TreeBuilder(repo, base);
    ifLeaf.remove("d", "ifLeaf");
    expect(ifLeaf.write().entryByPath("d/x.txt")?.id).toBe(blobId);
  });

  test("get() overlays recorded edits on the base tree", () => {
    const { treeId, blobId } = makeEntryTree();
    const other = repo.writeBlob(Buffer.from("other\n"));

    const builder = new TreeBuilder(repo, treeId);
    // Base fallback before any edit.
    expect(builder.get("file.txt")?.id).toBe(blobId);
    expect(builder.get("missing.txt")).toBeNull();

    // Last upsert wins.
    builder.upsert("file.txt", other);
    expect(builder.get("file.txt")?.id).toBe(other);
    expect(builder.get("file.txt")?.filename).toBe("file.txt");

    // Removing an ancestor hides everything below it.
    const nested = Tree.build(repo);
    const deep = repo.writeBlob(Buffer.from("deep\n"));
    nested.upsert("a/b/c.txt", deep);
    expect(nested.get("a/b/c.txt")?.id).toBe(deep);
    nested.remove("a");
    expect(nested.get("a/b/c.txt")).toBeNull();
  });

  test("clear() resets to the empty tree base and drops edits", () => {
    const blobId = repo.writeBlob(Buffer.from("to be cleared\n"));
    const builder = Tree.build(repo);
    builder.upsert("file.txt", blobId);
    builder.clear();
    expect(builder.get("file.txt")).toBeNull();
    expect(builder.write().id).toBe(repo.emptyTree().id);
  });

  test("Tree.build() with a base tree id matches new TreeBuilder()", () => {
    const { treeId, blobId } = makeEntryTree();
    const viaStatic = Tree.build(repo, treeId);
    viaStatic.upsert("extra.txt", blobId);
    const viaCtor = new TreeBuilder(repo, treeId);
    viaCtor.upsert("extra.txt", blobId);
    expect(viaStatic.write().id).toBe(viaCtor.write().id);
  });
});
