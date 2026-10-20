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
  rmSync(base, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 200,
  });
});

describe("Repository objects", () => {
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

  test("objectExists() reports existing and missing objects", () => {
    expect(repo.objectExists(repo.headId())).toBe(true);
    expect(repo.objectExists("0".repeat(40))).toBe(false);
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

  test("writeObject() and readObject() round-trip raw data, and computeHash() matches", () => {
    const data = Buffer.from("raw roundtrip\n");
    const id = repo.writeObject("blob", data);
    const obj = repo.readObject(id);
    expect(obj.kind).toBe("blob");
    expect(Buffer.from(obj.data).equals(data)).toBe(true);
    expect(repo.computeHash("blob", data)).toBe(id);
  });

  test("GitObject.data() returns the raw object body", () => {
    const id = repo.writeBlob(Buffer.from("gitobject data\n"));
    const obj = repo.findObject(id);
    expect(obj.kind).toBe("blob");
    expect(obj.data().toString()).toBe("gitobject data\n");
  });

  test("tryFindHeader() returns null for a missing object", () => {
    expect(repo.tryFindHeader("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef")).toBe(null);
  });

  test("object cache size helpers do not throw", () => {
    expect(typeof repo.computeObjectCacheSizeForTreeDiffs()).toBe("number");
    expect(() => repo.objectCacheSize(1024)).not.toThrow();
    expect(() => repo.objectCacheSizeIfUnset(2048)).not.toThrow();
    expect(() => repo.withObjectMemory()).not.toThrow();
  });
});
