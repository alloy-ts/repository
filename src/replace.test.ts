import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";

import { Repository } from "./main.ts";

let dir: string;
let repo: Repository;

beforeAll(() => {
  dir = mkdtempSync(join(tmpdir(), "repo-replace-"));
  execSync("git init -q -b main", { cwd: dir, stdio: "ignore" });
  writeFileSync(join(dir, "README.md"), "hello original\n");
  execSync("git -c user.email=author@example.com -c user.name=Author add -A", {
    cwd: dir,
    stdio: "ignore",
  });
  execSync(
    "git -c user.email=author@example.com -c user.name=Author -c commit.gpgsign=false commit -q -m init",
    {
      cwd: dir,
      stdio: "ignore",
    },
  );
  repo = Repository.open(dir);
});

afterAll(() => {
  rmSync(dir, { recursive: true, force: true });
});

describe("Repository.replace / tryReplace (src/replace.rs)", () => {
  test("replaces a blob and creates ReplaceManifest and Replace node with metadata", () => {
    const origBlobId = repo.writeBlob(Buffer.from("initial text"));
    expect(origBlobId).toMatch(/^[0-9a-f]{40}$/);

    const newOid = repo.replace(origBlobId, "replaced text v1", {
      authorName: "Alice",
      authorEmail: "alice@example.com",
    });

    expect(newOid).toMatch(/^[0-9a-f]{40}$/);
    expect(newOid).not.toBe(origBlobId);

    // Reading the original blob ID resolves through replacement ref to new content
    const blobContent = repo.findBlob(origBlobId).content().toString();
    expect(blobContent).toBe("replaced text v1");

    // Fetch ReplaceManifest at refs/replace/{origBlobId}
    const manifest = repo.getReplaceManifest(origBlobId);
    expect(manifest).not.toBeNull();
    expect(manifest!.aid).toBe(origBlobId);
    expect(manifest!.id).toBe(origBlobId);
    expect(manifest!.head).toBe(newOid);
    expect(manifest!.type).toBe("blob");
    expect(Array.isArray(manifest!.revisions)).toBe(true);
    expect(manifest!.revisions.length).toBe(1);

    const rev = manifest!.revisions[0];
    expect(rev.this).toBe(newOid);
    expect(rev.prev ?? null).toBeNull();
    expect(rev.next ?? null).toBeNull();
    expect(rev.head).toBe(newOid);
    expect(rev.authorName).toBe("Alice");
    expect(rev.authorEmail).toBe("alice@example.com");

    // Fetch individual Replace revision node at refs/replace/{newOid}
    const node = repo.getReplaceRevision(newOid);
    expect(node).not.toBeNull();
    expect(node!.this).toBe(newOid);
    expect(node!.authorName).toBe("Alice");
  });

  test("chains multiple replacement revisions for the same initial OID into a single ReplaceManifest", () => {
    const origBlobId = repo.writeBlob(Buffer.from("multi rev base"));

    const newOid1 = repo.replace(origBlobId, "multi rev v1");
    expect(newOid1).toMatch(/^[0-9a-f]{40}$/);

    // Second replacement requires force: true since replace ref already exists
    const newOid2 = repo.replace(origBlobId, "multi rev v2", { force: true });
    expect(newOid2).toMatch(/^[0-9a-f]{40}$/);

    // Third replacement
    const newOid3 = repo.replace(origBlobId, "multi rev v3", { force: true });
    expect(newOid3).toMatch(/^[0-9a-f]{40}$/);

    const manifest = repo.getReplaceManifest(origBlobId);
    expect(manifest).not.toBeNull();
    expect(manifest!.revisions.length).toBe(3);
    expect(manifest!.head).toBe(newOid3);

    const [rev1, rev2, rev3] = manifest!.revisions;
    expect(rev1.this).toBe(newOid1);
    expect(rev1.next).toBe(newOid2);
    expect(rev1.head).toBe(newOid3);

    expect(rev2.this).toBe(newOid2);
    expect(rev2.prev).toBe(newOid1);
    expect(rev2.next).toBe(newOid3);
    expect(rev2.head).toBe(newOid3);

    expect(rev3.this).toBe(newOid3);
    expect(rev3.prev).toBe(newOid2);
    expect(rev3.next ?? null).toBeNull();
    expect(rev3.head).toBe(newOid3);

    // Reading original blob resolves to latest (v3) content
    expect(repo.findBlob(origBlobId).content().toString()).toBe("multi rev v3");
  });

  test("supports replacing committed entry objects by path and extracts commit metadata", () => {
    const headTree = repo.headTree();
    const entry = headTree.entryByPath("README.md");
    expect(entry).not.toBeNull();
    const readmeOid = entry!.id;

    const newOid = repo.replace("README.md", "hello replaced readme entry\n", {
      authorName: "Editor",
      authorEmail: "editor@example.com",
    });

    expect(newOid).toMatch(/^[0-9a-f]{40}$/);

    const manifest = repo.getReplaceManifest("README.md");
    expect(manifest).not.toBeNull();
    expect(manifest!.path).toBe("README.md");
    expect(manifest!.type).toBe("entry");
    expect(manifest!.id).toBe(readmeOid);
    expect(manifest!.aid).toBe(readmeOid);

    // Verify commit metadata extraction from HEAD commit
    const rev = manifest!.revisions[0];
    expect(rev.authorName).toBe("Editor");
    expect(rev.authorEmail).toBe("editor@example.com");
    expect(typeof rev.committerName).toBe("string");
    expect(typeof rev.committerEmail).toBe("string");
    expect(typeof rev.updated_at).toBe("string");
  });

  test("supports replacing tree objects", () => {
    const emptyTree = repo.emptyTree();
    const emptyTreeId = emptyTree.id;

    // Create a non-empty tree by writing an entry
    const newTreeId = repo.writeTree([
      { path: "file.txt", mode: "100644", id: repo.emptyBlob().id },
    ]);

    const readTreeRaw = repo.readObject(newTreeId).data;

    const replaceId = repo.replace(emptyTreeId, readTreeRaw, { kind: "tree" });
    expect(replaceId).toBe(newTreeId);

    const manifest = repo.getReplaceManifest(emptyTreeId);
    expect(manifest).not.toBeNull();
    expect(manifest!.type).toBe("tree");
    expect(manifest!.head).toBe(newTreeId);

    // Reading object for emptyTreeId returns replaced tree data
    const obj = repo.readObject(emptyTreeId);
    expect(obj.kind).toBe("tree");
    expect(obj.data.length).toBe(readTreeRaw.length);
  });

  test("supports replacing commit objects and extracts commit author/committer metadata", () => {
    const headCommit = repo.headCommit();
    const headCommitId = headCommit.id;

    const commitRaw = repo.readObject(headCommitId).data;

    // Write a second commit object
    const newCommitId = repo.writeObject("commit", commitRaw);

    const replaceId = repo.replace(headCommitId, commitRaw, { kind: "commit" });
    expect(replaceId).toBe(newCommitId);

    const manifest = repo.getReplaceManifest(headCommitId);
    expect(manifest).not.toBeNull();
    expect(manifest!.type).toBe("commit");
    expect(manifest!.head).toBe(newCommitId);

    const rev = manifest!.revisions[0];
    expect(rev.authorName).toBe("Author");
    expect(rev.authorEmail).toBe("author@example.com");
  });

  test("supports replacing tag objects", () => {
    const tagRef = repo.createTag("v1.0.0", repo.headId(), "release v1.0.0");
    const tagOid = tagRef.target;

    const tagRaw = repo.readObject(tagOid).data;
    const newTagId = repo.writeObject("tag", tagRaw);

    const replaceId = repo.replace(tagOid, tagRaw, { kind: "tag" });
    expect(replaceId).toBe(newTagId);

    const manifest = repo.getReplaceManifest(tagOid);
    expect(manifest).not.toBeNull();
    expect(manifest!.type).toBe("tag");
    expect(manifest!.head).toBe(newTagId);
  });

  test("rejects replacing without force when replace reference already exists", () => {
    const origBlobId = repo.writeBlob(Buffer.from("force test base"));
    repo.replace(origBlobId, "force test v1");

    expect(() => {
      repo.replace(origBlobId, "force test v2");
    }).toThrow();
  });

  test("rejects replacing with mismatched object type unless force is passed", () => {
    const origBlobId = repo.writeBlob(Buffer.from("type mismatch base"));
    const headCommitData = repo.readObject(repo.headId()).data;

    expect(() => {
      // Trying to replace a blob with a commit object without force throws
      repo.replace(origBlobId, headCommitData, { kind: "commit" });
    }).toThrow();

    // With force: true, cross-type replacement succeeds
    const replaceId = repo.replace(origBlobId, headCommitData, {
      kind: "commit",
      force: true,
    });
    expect(replaceId).toMatch(/^[0-9a-f]{40}$/);
  });

  test("tryReplace returns null on failure instead of throwing", () => {
    const origBlobId = repo.writeBlob(Buffer.from("tryReplace base"));
    repo.replace(origBlobId, "tryReplace v1");

    // Second replace without force fails and tryReplace returns null
    const res = repo.tryReplace(origBlobId, "tryReplace v2");
    expect(res).toBeNull();
  });

  test("honors GIT_NO_REPLACE_OBJECTS environment variable", () => {
    process.env.GIT_NO_REPLACE_OBJECTS = "1";

    const origBlobId = repo.writeBlob(Buffer.from("config toggle base"));
    const result = repo.replace(origBlobId, "direct replaced text");

    // When disabled, returns target and does not create refs/replace
    expect(result).toBe(origBlobId);
    expect(repo.getReplaceManifest(origBlobId)).toBeNull();

    delete process.env.GIT_NO_REPLACE_OBJECTS;
  });
});
