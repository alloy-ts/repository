import { execSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository, type Tree, type TreeEntry } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-object-"));
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
  rmSync(base, { recursive: true, force: true });
});

function firstBlobEntry(r: Repository, tree: Tree): TreeEntry | null {
  for (const e of tree.entries) {
    if (e.mode === "100644" || e.mode === "100755" || e.mode === "120000") {
      return e;
    }
    if (e.mode === "040000") {
      const sub = r.findTree(e.id);
      const found = firstBlobEntry(r, sub);
      if (found) return found;
    }
  }
  return null;
}

describe("Repository objects", () => {
  test("findTree resolves the commit's tree", () => {
    const tree = repo.findTree(repo.headCommit().treeId);
    expect(tree.id).toBe(repo.headCommit().treeId);
    expect(Array.isArray(tree.entries)).toBe(true);
  });

  test("findBlob resolves a real blob via content()", () => {
    const entry = firstBlobEntry(repo, repo.findTree(repo.headCommit().treeId));
    if (entry) {
      const blob = repo.findBlob(entry.id);
      expect(Buffer.isBuffer(blob.content())).toBe(true);
    }
  });

  test("findObject returns a kinded wrapper with downcasts", () => {
    const commit = repo.headCommit();
    const asCommit = repo.findObject(commit.id);
    expect(asCommit.kind).toBe("commit");
    expect(asCommit.asCommit()?.id).toBe(commit.id);
    expect(asCommit.asTree()).toBeNull();

    const tree = repo.findTree(commit.treeId);
    const asTree = repo.findObject(tree.id);
    expect(asTree.kind).toBe("tree");
    expect(asTree.asTree()?.id).toBe(tree.id);
    expect(asTree.asCommit()).toBeNull();
  });

  test("objectExists and writeBlob round-trip", () => {
    expect(repo.objectExists(repo.headId())).toBe(true);
    expect(repo.objectExists("0".repeat(40))).toBe(false);

    const id = repo.writeBlob(Buffer.from("probe blob content\n"));
    expect(repo.objectExists(id)).toBe(true);
    expect(repo.findBlob(id).content().toString()).toBe("probe blob content\n");
  });
});

describe("Object writing & object retrieval (Wave 4)", () => {
  test("writeTag() and peel() roundtrip", () => {
    const head = repo.headCommit();
    const newCommitId = repo.writeCommit({
      tree: head.treeId,
      parents: [head.id],
      message: "custom loose commit message\n",
    });

    const tagObjId = repo.writeTag({
      name: "v1.0.0-custom",
      target: newCommitId,
      targetKind: "commit",
      message: "custom annotated tag\n",
    });
    expect(typeof tagObjId).toBe("string");

    const peeledTree = repo.peel(tagObjId, "tree");
    expect(peeledTree).toBe(head.treeId);

    const peeledCommit = repo.peel(tagObjId, "commit");
    expect(peeledCommit).toBe(newCommitId);
  });

  test("writeTree() creates a tree and tryFindObject() retrieves it", () => {
    const blobId = repo.writeBlob(Buffer.from("sample content\n"));
    const treeId = repo.writeTree([
      {
        path: "file.txt",
        mode: "100644",
        id: blobId,
      },
    ]);
    expect(typeof treeId).toBe("string");
    expect(treeId).toMatch(/^[0-9a-f]{40}$/);

    const found = repo.tryFindObject(treeId);
    expect(found).not.toBeNull();
    expect(found?.kind).toBe("tree");

    const missing = repo.tryFindObject("0".repeat(40));
    expect(missing).toBeNull();
  });
});

describe("Objects / ODB (write_object, edit_tree, headers, empty)", () => {
  test("write_object() writes a raw object and findHeader reads its size", () => {
    const data = Buffer.from("hello world\n");
    const id = repo.writeObject("blob", data);
    expect(id).toMatch(/^[0-9a-f]{40}$/);
    const header = repo.findHeader(id);
    expect(header.kind).toBe("blob");
    expect(header.size).toBe(data.length);
  });

  test("writeBlobStream() writes a blob via the streaming path", () => {
    const id = repo.writeBlobStream(Buffer.from("streamed\n"));
    expect(id).toMatch(/^[0-9a-f]{40}$/);
  });

  test("tryFindHeader() returns null for a missing object", () => {
    expect(repo.tryFindHeader("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef")).toBe(null);
  });

  test("emptyTree() and emptyBlob() return the well-known ids", () => {
    expect(repo.emptyTree().id).toBe("4b825dc642cb6eb9a060e54bf8d69288fbee4904");
    expect(repo.emptyBlob().id).toBe("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
  });

  test("editTree() upserts an entry on top of the empty tree", () => {
    const blobId = repo.writeBlob(Buffer.from("x"));
    const treeId = repo.editTree(repo.emptyTree().id, [
      { path: "x.txt", mode: "100644", id: blobId },
    ]);
    expect(treeId).toMatch(/^[0-9a-f]{40}$/);
    const tree = repo.findTree(treeId);
    expect(tree.entries.length).toBe(1);
  });

  test("object cache size helpers do not throw", () => {
    expect(typeof repo.computeObjectCacheSizeForTreeDiffs()).toBe("number");
    expect(() => repo.objectCacheSize(1024)).not.toThrow();
    expect(() => repo.objectCacheSizeIfUnset(2048)).not.toThrow();
    expect(() => repo.withObjectMemory()).not.toThrow();
  });
});
