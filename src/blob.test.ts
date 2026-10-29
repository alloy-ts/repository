import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository, type Tree, type TreeEntry } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-blob-"));
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

describe("Repository blobs", () => {
  test("findBlob resolves a real blob via content()", () => {
    const entry = firstBlobEntry(repo, repo.findTree(repo.headCommit().treeId));
    if (entry) {
      const blob = repo.findBlob(entry.id);
      expect(blob.id).toBe(entry.id);
      expect(Buffer.isBuffer(blob.content())).toBe(true);
    }
  });

  test("writeBlob() and findBlob() round-trip", () => {
    const id = repo.writeBlob(Buffer.from("probe blob content\n"));
    expect(id).toMatch(/^[0-9a-f]{40}$/);
    expect(repo.objectExists(id)).toBe(true);
    expect(repo.findBlob(id).content().toString()).toBe("probe blob content\n");
  });

  test("writeBlobStream() writes a blob via the streaming path", () => {
    const id = repo.writeBlobStream(Buffer.from("streamed\n"));
    expect(id).toMatch(/^[0-9a-f]{40}$/);
  });

  test("emptyBlob() returns the well-known id", () => {
    expect(repo.emptyBlob().id).toBe("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
  });

  test("isBinary() detects NUL bytes in the first 8KB", () => {
    const binaryId = repo.writeBlob(Buffer.from([0, 1, 2, 0, 4]));
    expect(repo.findBlob(binaryId).isBinary()).toBe(true);

    const textId = repo.writeBlob(Buffer.from("plain text\n"));
    expect(repo.findBlob(textId).isBinary()).toBe(false);
  });

  test("size getter matches the content length", () => {
    const blob = repo.findBlob(repo.writeBlob(Buffer.from("12345")));
    expect(blob.size).toBe(5);
    expect(blob.content().length).toBe(blob.size);
  });

  test("asObject() exposes the same object as a Blob", () => {
    const id = repo.writeBlob(Buffer.from("object round-trip\n"));
    const object = repo.findBlob(id).asObject();
    expect(object.id).toBe(id);
    expect(object.asBlob()?.id).toBe(id);
  });

  test("writeBlobPath() hashes a file on disk", () => {
    const file = join(base, "written.txt");
    writeFileSync(file, "on-disk content\n");
    const id = repo.writeBlobPath(file);
    expect(id).toMatch(/^[0-9a-f]{40}$/);
    expect(repo.findBlob(id).content().toString()).toBe("on-disk content\n");
  });

  test("getBlob resolves a file at a path in HEAD (git show HEAD:path)", () => {
    const blob = repo.getBlob("HEAD", "Cargo.toml");
    expect(Buffer.isBuffer(blob.content())).toBe(true);
    expect(blob.content().toString()).toContain("[package]");
  });

  test("getBlob accepts a branch name and an object id (same blob either way)", () => {
    const byBranch = repo.getBlob("HEAD", "Cargo.toml");
    const byId = repo.getBlob(repo.headCommit().id, "Cargo.toml");
    expect(byId.id).toBe(byBranch.id);
  });

  test("getBlob throws for a missing path or a directory", () => {
    expect(() => repo.getBlob("HEAD", "does-not-exist.txt")).toThrow();
    expect(() => repo.getBlob("HEAD", "src")).toThrow();
  });

  test("getBlobAtRoot resolves a file at the root commit", () => {
    const blob = repo.getBlobAtRoot("HEAD", "repository.manifest.yml");
    expect(Buffer.isBuffer(blob.content())).toBe(true);
  });

  test("TreeEntry.toBlob(repo) returns the same object", () => {
    const tree = repo.findTree(repo.headCommit().treeId);
    const entry = tree.entries.find((e: TreeEntry) => e.mode === "100644" || e.mode === "100755");
    if (entry) {
      const blob = entry.toBlob(repo);
      expect(blob?.id).toBe(entry.id);
    }
  });

  test("Entry.toBlob() returns the same object", () => {
    const tree = repo.findTree(repo.headCommit().treeId);
    const entry = tree.intoEntries().find((e) => !e.isTree());
    if (entry) {
      const blob = entry.toBlob();
      expect(blob?.id).toBe(entry.id);
    }
  });
});
