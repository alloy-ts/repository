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
  base = mkdtempSync(join(tmpdir(), "repo-replace-"));
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

describe("Replace / Revision / ReplaceManifest NAPI bindings (src/replace.rs)", () => {
  test("getReplace returns null when no replace ref exists for aid", () => {
    const headId = repo.headId();
    expect(repo.getReplace(headId)).toBe(null);
  });

  test("setReplace creates replace ref and returns Replace node", () => {
    const headId = repo.headId();
    const blobId = repo.writeBlob(Buffer.from("replaced content\n"));

    const replaceNode = repo.setReplace(headId, blobId);
    expect(replaceNode.this).toBe(`refs/replace/${headId}`);
    expect(typeof replaceNode.authorName).toBe("string");
    expect(typeof replaceNode.authorEmail).toBe("string");
    expect(typeof replaceNode.updatedAt).toBe("string");

    const fetched = repo.getReplace(headId);
    expect(fetched).not.toBe(null);
    expect(fetched?.this).toBe(`refs/replace/${headId}`);
  });

  test("getRevision retrieves Replace node mapped to Revision", () => {
    const headId = repo.headId();
    const rev = repo.getRevision(headId);
    expect(rev).not.toBe(null);
    expect(rev?.this).toBe(`refs/replace/${headId}`);
  });

  test("replaceManifest returns complete ReplaceManifest for aid", () => {
    const headId = repo.headId();
    const manifest = repo.replaceManifest(headId, "commit");

    expect(manifest.id).toBe(headId);
    expect(manifest.refId).toBe(`refs/replace/${headId}`);
    expect(manifest.type).toBe("commit");
    expect(Array.isArray(manifest.revisions)).toBe(true);
    expect(manifest.revisions.length).toBe(1);
    expect(manifest.head).toBe(`refs/replace/${headId}`);
  });
});
