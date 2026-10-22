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
  base = mkdtempSync(join(tmpdir(), "repo-replace-types-"));
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
  writeFileSync(join(cloneDir, "entry_file.txt"), "entry initial content\n");
  git("add -A", cloneDir);
  git("config commit.gpgsign false", cloneDir);
  git('-c user.email=test@example.com -c user.name=Test commit -m "add entry file"', cloneDir);
  repo = Repository.open(cloneDir);
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true });
});

describe("Versioned replace for all object types (blob, tree, commit, tag, entry)", () => {
  test("replace for blob type", () => {
    const blobId = repo.writeBlob(Buffer.from("blob v1"));
    const manifest = repo.replace(blobId, Buffer.from("blob v2"));

    expect(manifest.$ref).toBe(blobId);
    expect(manifest.type).toBe("blob");
    expect(manifest.revisions.length).toBe(1);
    expect(manifest.revisions[0].this).toBe(manifest.head);
  });

  test("replace for entry type with path and commit metadata", () => {
    const entryBlobId = repo.writeBlob(Buffer.from("entry v1"));
    const manifest = repo.replace(entryBlobId, Buffer.from("entry v2"), "entry_file.txt");

    expect(manifest.$ref).toBe(entryBlobId);
    expect(manifest.type).toBe("entry");
    expect(manifest.path).toBe("entry_file.txt");
    expect(manifest.revisions.length).toBe(1);

    const rev = manifest.revisions[0];
    expect(typeof rev.authorName).toBe("string");
    expect(typeof rev.authorEmail).toBe("string");
    expect(rev.committerName === undefined || typeof rev.committerName === "string").toBe(true);
  });

  test("replace for tree type", () => {
    const treeId = repo.writeTree([]);
    const manifest = repo.replace(treeId, Buffer.from("tree replacement payload"));

    expect(manifest.$ref).toBe(treeId);
    expect(manifest.type).toBe("tree");
    expect(manifest.revisions.length).toBe(1);
  });

  test("replace for commit type", () => {
    const headCommitId = repo.headId();
    const manifest = repo.replace(headCommitId, Buffer.from("commit replacement payload"));

    expect(manifest.$ref).toBe(headCommitId);
    expect(manifest.type).toBe("commit");
    expect(manifest.revisions.length).toBe(1);
  });

  test("replace for tag type", () => {
    const headId = repo.headId();
    const tagId = repo.writeTag({
      name: "v1.0.0-test",
      target: headId,
      targetKind: "commit",
      message: "release v1.0.0",
    });
    const manifest = repo.replace(tagId, Buffer.from("tag replacement payload"));

    expect(manifest.$ref).toBe(tagId);
    expect(manifest.type).toBe("tag");
    expect(manifest.revisions.length).toBe(1);
  });
});
