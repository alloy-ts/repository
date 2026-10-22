import { execSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vitest";
import { ReplaceManifest, Repository, Revision } from "../dist/index.js";

let dir: string;
let repo: Repository;
let initialCommitId: string;
let initialTreeId: string;
let readmeBlobId: string;

beforeAll(() => {
  dir = mkdtempSync(join(tmpdir(), "repo-replace-"));
  execSync("git init -q -b main", { cwd: dir, stdio: "ignore" });
  writeFileSync(join(dir, "README.md"), "# Original Content\n");
  mkdirSync(join(dir, "src"), { recursive: true });
  writeFileSync(join(dir, "src", "index.ts"), "console.log('original');\n");

  execSync("git add -A", { cwd: dir, stdio: "ignore" });
  execSync("git commit -q -m 'initial commit'", {
    cwd: dir,
    stdio: "ignore",
    env: {
      ...process.env,
      GIT_AUTHOR_NAME: "Test Author",
      GIT_AUTHOR_EMAIL: "author@example.com",
      GIT_COMMITTER_NAME: "Test Committer",
      GIT_COMMITTER_EMAIL: "committer@example.com",
    },
  });

  repo = Repository.open(dir);
  initialCommitId = repo.headCommit().id;
  initialTreeId = repo.headCommit().treeId;
  const entry = repo.findTree(initialTreeId).entryByPath("README.md");
  readmeBlobId = entry!.id;
});

afterAll(() => {
  rmSync(dir, { recursive: true, force: true });
});

describe("Replace / Revision & ReplaceManifest", () => {
  test("creates a Revision object with properties", () => {
    const rev: Revision = {
      originalOid: "1111111111111111111111111111111111111111",
      replacementOid: "2222222222222222222222222222222222222222",
      objectType: "blob",
    };

    expect(rev.originalOid).toBe("1111111111111111111111111111111111111111");
    expect(rev.replacementOid).toBe("2222222222222222222222222222222222222222");
    expect(rev.objectType).toBe("blob");
  });

  test("creates and manages ReplaceManifest", () => {
    const rev1: Revision = {
      originalOid: "aaaa",
      replacementOid: "bbbb",
      objectType: "entry",
    };
    const rev2: Revision = {
      originalOid: "cccc",
      replacementOid: "dddd",
      objectType: "entry",
    };

    const manifest = new ReplaceManifest("aaaa", [rev1]);
    expect(manifest.canonicalOid).toBe("aaaa");
    expect(manifest.length).toBe(1);
    expect(manifest.replaces).toEqual([rev1]);
    expect(manifest.latest).toEqual(rev1);

    manifest.add(rev2);
    expect(manifest.length).toBe(2);
    expect(manifest.latest).toEqual(rev2);
  });

  test("replace_object works for blob, tree, commit, and entry object types", () => {
    // 1. Blob replacement
    const replacementBlobId = repo.writeObject("blob", Buffer.from("# Replaced Content\n"));
    const blobManifest = repo.replaceObject(readmeBlobId, replacementBlobId, "blob");
    expect(blobManifest.canonicalOid).toBe(readmeBlobId);
    expect(blobManifest.latest?.replacementOid).toBe(replacementBlobId);

    // 2. Tree replacement
    const replacementTreeId = repo.emptyTree().id;
    const treeManifest = repo.replaceObject(initialTreeId, replacementTreeId, "tree");
    expect(treeManifest.canonicalOid).toBe(initialTreeId);
    expect(treeManifest.latest?.replacementOid).toBe(replacementTreeId);

    // 3. Commit replacement
    const replacementCommitId = repo.writeCommit({
      message: "Replaced Commit\n",
      tree: replacementTreeId,
      parents: [initialCommitId],
    });
    const commitManifest = repo.replaceObject(initialCommitId, replacementCommitId, "commit");
    expect(commitManifest.canonicalOid).toBe(initialCommitId);
    expect(commitManifest.latest?.replacementOid).toBe(replacementCommitId);

    // 4. Entry replacement
    const entry = repo.findTree(initialTreeId).entryByPath("README.md");
    expect(entry).toBeDefined();
    expect(entry!.objectType).toBe("entry");

    const entryManifest = repo.replaceObject(entry!.id, replacementBlobId, "entry");
    expect(entryManifest.canonicalOid).toBe(entry!.id);
  });

  test("retrieves commit metadata (committer/author name, email, timestamp)", () => {
    const meta = repo.getCommitMetadata(initialCommitId);
    expect(meta).toBeDefined();
    expect(meta?.authorName).toBe("Test Author");
    expect(meta?.authorEmail).toBe("author@example.com");
    expect(meta?.committerName).toBe("Test Committer");
    expect(meta?.committerEmail).toBe("committer@example.com");
    expect(typeof meta?.commitTimestamp).toBe("number");
  });

  test("replacing same OID multiple times builds a revision history under canonical initOid", () => {
    const initOid = repo.writeObject("blob", Buffer.from("v0"));
    const rep1 = repo.writeObject("blob", Buffer.from("v1"));
    const rep2 = repo.writeObject("blob", Buffer.from("v2"));
    const rep3 = repo.writeObject("blob", Buffer.from("v3"));

    repo.replaceObject(initOid, rep1, "entry");
    repo.replaceObject(initOid, rep2, "entry");
    const finalManifest = repo.replaceObject(initOid, rep3, "entry");

    expect(finalManifest.canonicalOid).toBe(initOid);
    expect(finalManifest.length).toBe(3);
    expect(finalManifest.replaces.map((r) => r.replacementOid)).toEqual([rep1, rep2, rep3]);
    expect(finalManifest.latest?.replacementOid).toBe(rep3);

    // Verify canonical initOid reference stays stable
    const readManifest = repo.replaceManifest(initOid);
    expect(readManifest.canonicalOid).toBe(initOid);
    expect(readManifest.latest?.replacementOid).toBe(rep3);
  });
});
