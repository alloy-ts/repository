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
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
});

describe("Repository replace / try_replace (draft)", () => {
  test("replace() records a revision and catFile returns the latest content", () => {
    const id = repo.writeBlob(Buffer.from("original-contents"));
    const out = repo.replace(id, Buffer.from("replaced-contents"), { message: "v1" });
    expect(out.replacementId.length).toBe(40);
    expect(out.reference).toBe(`refs/replace/${id}`);
    expect(out.manifest.id).toBe(id);
    expect(out.manifest.revisions.length).toBe(1);
    expect(out.manifest.head).toBe(out.replacementId);
    // First revision node points at the new content; no previous revision yet.
    expect(out.manifest.revisions[0].this_ref).toBe(out.replacementId);
    expect(out.manifest.revisions[0].prev).toBe(null);
    expect(out.manifest.revisions[0].head).toBe(out.replacementId);

    // Reading the original oid now yields the replacement content.
    const readBack = repo.catFile(id) as Buffer;
    expect(readBack.toString("utf8")).toBe("replaced-contents");

    // getReference follows the replacement and returns the latest content id.
    expect(repo.getReference(id)).toBe(out.replacementId);
  });

  test("try_replace() returns null when a replace ref already exists", () => {
    const id = repo.writeBlob(Buffer.from("first-version"));
    repo.replace(id, Buffer.from("second-version"));
    const retry = repo.try_replace(id, Buffer.from("third-version"));
    expect(retry).toBe(null);
  });

  test("replace({ force: true }) appends another revision", () => {
    const id = repo.writeBlob(Buffer.from("v0"));
    const r1 = repo.replace(id, Buffer.from("v1"));
    const r2 = repo.replace(id, Buffer.from("v2"), { force: true });
    expect(r2.manifest.revisions.length).toBe(r1.manifest.revisions.length + 1);
    expect(r2.manifest.head).toBe(r2.replacementId);
    expect((repo.catFile(id) as Buffer).toString("utf8")).toBe("v2");
  });

  test("replace with a string content works the same as a Buffer", () => {
    const id = repo.writeBlob(Buffer.from("blob-A"));
    const out = repo.replace(id, "blob-B-string");
    expect((repo.catFile(id) as Buffer).toString("utf8")).toBe("blob-B-string");
    expect(out.replacementId.length).toBe(40);
  });
});
