import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

function git(args: string, cwd: string) {
  execSync(`git ${args}`, { cwd, stdio: "ignore" });
}

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-replace-"));
  cloneDir = join(base, "clone");
  try {
    repo = await Repository.clone(realRoot, cloneDir, false);
  } catch (e) {
    rmSync(cloneDir, { recursive: true, force: true });
    execSync(`git clone --local --quiet ${JSON.stringify(realRoot)} ${JSON.stringify(cloneDir)}`, {
      stdio: "ignore",
    });
    repo = Repository.open(cloneDir);
  }
  writeFileSync(join(cloneDir, "versioned.txt"), "version 1 content\n");
  git("add -A", cloneDir);
  git("config commit.gpgsign false", cloneDir);
  git('-c user.email=test@example.com -c user.name=Test commit -m "add versioned file"', cloneDir);
  repo = Repository.open(cloneDir);
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true });
});

describe("Versioned object replace API (src/replace.rs)", () => {
  test("single object replacement generates Revision node and ReplaceManifest", () => {
    const blobId = repo.writeBlob(Buffer.from("version 1 initial"));
    const manifest = repo.replace(blobId, Buffer.from("version 2 replaced"), "versioned.txt");

    expect(manifest.$ref).toBe(blobId);
    expect(manifest.path).toBe("versioned.txt");
    expect(manifest.type).toBe("entry");
    expect(manifest.revisions.length).toBe(1);

    const firstRev = manifest.revisions[0];
    expect(typeof firstRev.this).toBe("string");
    expect(firstRev.prev).toBeUndefined();
    expect(typeof firstRev.authorName).toBe("string");
    expect(typeof firstRev.updatedAt).toBe("string");
  });

  test("multiple object replacements build revision chain with prev/next/head pointers", () => {
    const origId = repo.writeBlob(Buffer.from("base object"));
    const manifest1 = repo.replace(origId, Buffer.from("v2 object"));
    const v2Oid = manifest1.head;

    const manifest2 = repo.replace(origId, Buffer.from("v3 object"));
    const v3Oid = manifest2.head;

    expect(manifest2.revisions.length).toBe(2);
    expect(manifest2.head).toBe(v3Oid);

    const node1 = repo.getReplaceNode(v2Oid);
    expect(node1).not.toBeNull();
    expect(node1?.this).toBe(v2Oid);
    expect(node1?.next).toBe(v3Oid);

    const node2 = repo.getReplaceNode(v3Oid);
    expect(node2).not.toBeNull();
    expect(node2?.this).toBe(v3Oid);
    expect(node2?.prev).toBe(v2Oid);
    expect(node2?.head).toBe(v3Oid);
  });

  test("Object instance method replace delegates to Repository.replace", () => {
    const blobId = repo.writeBlob(Buffer.from("object instance v1"));
    const obj = repo.findObject(blobId);
    const manifest = obj.replace(Buffer.from("object instance v2"), undefined);

    expect(manifest.$ref).toBe(blobId);
    expect(manifest.revisions.length).toBe(1);
    expect(manifest.head).toBe(manifest.revisions[0].this);
  });
});
