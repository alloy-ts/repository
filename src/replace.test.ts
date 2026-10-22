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

function git(args: string, cwd: string) {
  execSync(`git ${args}`, { cwd, stdio: "ignore" });
}

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-replace-"));
  cloneDir = join(base, "clone");
  try {
    repo = await Repository.clone(realRoot, cloneDir, false);
  } catch {
    execSync(`git clone --local --quiet ${JSON.stringify(realRoot)} ${JSON.stringify(cloneDir)}`, {
      stdio: "ignore",
    });
    repo = Repository.open(cloneDir);
  }
  git("config commit.gpgsign false", cloneDir);
  git("config user.name 'Test Author'", cloneDir);
  git("config user.email 'author@example.com'", cloneDir);
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
});

describe("Repository replace / try_replace versioned history", () => {
  test("replace() records a revision node and manifest, catFile returns latest content", () => {
    const origId = repo.writeBlob(Buffer.from("original-contents"));
    const out = repo.replace(origId, Buffer.from("replaced-v1"), { message: "v1 replace" });

    expect(out.replacementId.length).toBe(40);
    expect(out.reference).toBe(`refs/replace/${origId}`);
    expect(out.manifest.id).toBe(origId);
    expect(out.manifest.type).toBe("blob");
    expect(out.manifest.head).toBe(out.replacementId);

    // Flat revisions array has 1 Revision item.
    expect(out.manifest.revisions.length).toBe(1);
    const rev1 = out.manifest.revisions[0];
    expect(rev1.this).toBe(out.replacementId);
    expect(rev1.prev).toBe(null);
    expect(rev1.next).toBe(null);
    expect(rev1.head).toBe(out.replacementId);
    expect(rev1.authorName).toBe("Test Author");
    expect(rev1.authorEmail).toBe("author@example.com");
    expect(typeof rev1.updated_at).toBe("string");

    // Reading original OID yields replacement content
    const readBack = repo.catFile(origId) as Buffer;
    expect(readBack.toString("utf8")).toBe("replaced-v1");

    // getReference follows replacement
    expect(repo.getReference(origId)).toBe(out.replacementId);
  });

  test("multiple replacements append to revisions and update prev/next links", () => {
    const origId = repo.writeBlob(Buffer.from("v0-base"));

    // First replacement (v1)
    const r1 = repo.replace(origId, Buffer.from("v1-content"), { message: "v1" });
    const v1Id = r1.replacementId;

    // Second replacement (v2) with force: true
    const r2 = repo.replace(origId, Buffer.from("v2-content"), { force: true, message: "v2" });
    const v2Id = r2.replacementId;

    // Third replacement (v3) with force: true
    const r3 = repo.replace(origId, Buffer.from("v3-content"), { force: true, message: "v3" });
    const v3Id = r3.replacementId;

    // Verify root ReplaceManifest at refs/replace/{origId}
    const manifest = r3.manifest;
    expect(manifest.id).toBe(origId);
    expect(manifest.head).toBe(v3Id);
    expect(manifest.revisions.length).toBe(3);

    // Revision 1
    expect(manifest.revisions[0].this).toBe(v1Id);
    expect(manifest.revisions[0].prev).toBe(null);
    expect(manifest.revisions[0].next).toBe(v2Id);

    // Revision 2
    expect(manifest.revisions[1].this).toBe(v2Id);
    expect(manifest.revisions[1].prev).toBe(v1Id);
    expect(manifest.revisions[1].next).toBe(v3Id);

    // Revision 3
    expect(manifest.revisions[2].this).toBe(v3Id);
    expect(manifest.revisions[2].prev).toBe(v2Id);
    expect(manifest.revisions[2].next).toBe(null);
    expect(manifest.revisions[2].head).toBe(v3Id);

    // Verify catFile(origId) returns latest v3 content
    expect((repo.catFile(origId) as Buffer).toString("utf8")).toBe("v3-content");

    // Verify individual revision metadata nodes stored at refs/replace/{replaceOid}
    const node1Blob = repo.catFile(v1Id) as Buffer;
    const node1 = JSON.parse(node1Blob.toString("utf8"));
    expect(node1.this).toBe(v1Id);
    expect(node1.prev).toBe(null);
    expect(node1.next).toBe(v2Id);

    const node2Blob = repo.catFile(v2Id) as Buffer;
    const node2 = JSON.parse(node2Blob.toString("utf8"));
    expect(node2.this).toBe(v2Id);
    expect(node2.prev).toBe(v1Id);
    expect(node2.next).toBe(v3Id);

    const node3Blob = repo.catFile(v3Id) as Buffer;
    const node3 = JSON.parse(node3Blob.toString("utf8"));
    expect(node3.this).toBe(v3Id);
    expect(node3.prev).toBe(v2Id);
    expect(node3.next).toBe(null);
  });

  test("GitObject instance method replace() works identically", () => {
    const origId = repo.writeBlob(Buffer.from("gitobject-v0"));
    const obj = repo.findObject(origId);
    expect(obj.id).toBe(origId);

    const res = obj.replace("gitobject-v1");
    expect(res.replacementId.length).toBe(40);
    expect(res.manifest.revisions.length).toBe(1);
    expect((repo.catFile(origId) as Buffer).toString("utf8")).toBe("gitobject-v1");
  });

  test("try_replace() returns null when replace ref exists and force is omitted", () => {
    const id = repo.writeBlob(Buffer.from("first-version"));
    repo.replace(id, Buffer.from("second-version"));

    const retry = repo.try_replace(id, Buffer.from("third-version"));
    expect(retry).toBe(null);
  });

  test("replace with string content works as expected", () => {
    const id = repo.writeBlob(Buffer.from("blob-A"));
    const out = repo.replace(id, "blob-B-string");
    expect((repo.catFile(id) as Buffer).toString("utf8")).toBe("blob-B-string");
    expect(out.replacementId.length).toBe(40);
  });
});
