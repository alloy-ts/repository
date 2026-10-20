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
  base = mkdtempSync(join(tmpdir(), "repo-blob-"));
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
});
