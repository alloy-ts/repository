import { describe, expect, test } from "vite-plus/test";
import { Repository } from "./main.ts";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { mkdirSync } from "node:fs";

function setupRepo() {
  const dir = join(tmpdir(), `test-replace-${Math.random().toString(36).substring(2, 9)}`);
  mkdirSync(dir, { recursive: true });
  const repo = Repository.init(dir, false);
  return { repo, dir };
}

describe("Repository.replaceObject", () => {
  test("replaces blob object and writes replace refs and manifest", () => {
    const { repo } = setupRepo();
    const initBlobId = repo.writeBlob(Buffer.from("initial blob content"));

    const manifest = repo.replaceObject(initBlobId, "blob", {
      content: Buffer.from("replaced blob content v1"),
    });

    expect(manifest.initOid).toBe(initBlobId);
    expect(manifest.latestReplaceOid).toBeDefined();
    expect(manifest.latestReplaceOid).not.toBe(initBlobId);
    expect(manifest.kind).toBe("blob");
    expect(manifest.history.length).toBe(1);
    expect(manifest.history[0].initOid).toBe(initBlobId);
    expect(manifest.history[0].replaceOid).toBe(manifest.latestReplaceOid);

    const fetchedManifest = repo.getReplaceManifest(initBlobId);
    expect(fetchedManifest).toBeDefined();
    expect(fetchedManifest?.latestReplaceOid).toBe(manifest.latestReplaceOid);
  });

  test("replaces tree object", () => {
    const { repo } = setupRepo();
    const blob1 = repo.writeBlob(Buffer.from("file1"));
    const initTreeId = repo.writeTree([{ path: "file1.txt", mode: "100644", id: blob1 }]);

    const blob2 = repo.writeBlob(Buffer.from("file1 modified"));
    const manifest = repo.replaceObject(initTreeId, "tree", {
      entries: [{ path: "file1.txt", mode: "100644", id: blob2 }],
    });

    expect(manifest.initOid).toBe(initTreeId);
    expect(manifest.kind).toBe("tree");
    expect(manifest.latestReplaceOid).toBeDefined();
    expect(manifest.latestReplaceOid).not.toBe(initTreeId);
  });

  test("replaces commit object and extracts commit metadata", () => {
    const { repo } = setupRepo();
    const blobId = repo.writeBlob(Buffer.from("init"));
    const treeId = repo.writeTree([{ path: "init.txt", mode: "100644", id: blobId }]);

    const commitId = repo.commitAs(
      { name: "Committer Alice", email: "alice@example.com", time: 1700000000, timezoneOffset: 0 },
      { name: "Author Bob", email: "bob@example.com", time: 1700000000, timezoneOffset: 0 },
      "HEAD",
      "Initial commit",
      treeId,
      [],
    );

    const manifest = repo.replaceObject(commitId, "commit", {
      content: Buffer.from("replaced commit content"),
    });

    expect(manifest.initOid).toBe(commitId);
    expect(manifest.kind).toBe("commit");
    expect(manifest.history[0].authorName).toBe("Author Bob");
    expect(manifest.history[0].authorEmail).toBe("bob@example.com");
    expect(manifest.history[0].committerName).toBe("Committer Alice");
    expect(manifest.history[0].committerEmail).toBe("alice@example.com");
  });

  test("replaces tag object", () => {
    const { repo } = setupRepo();
    const blobId = repo.writeBlob(Buffer.from("tag target"));
    const tagId = repo.writeBlob(Buffer.from(`object ${blobId}\ntype blob\ntag v1.0\n`));

    const manifest = repo.replaceObject(tagId, "tag", {
      content: Buffer.from("replaced tag content"),
    });

    expect(manifest.initOid).toBe(tagId);
    expect(manifest.kind).toBe("tag");
    expect(manifest.latestReplaceOid).toBeDefined();
  });

  test("replaces entry object at a fixed path with commit metadata", () => {
    const { repo } = setupRepo();
    const blobId = repo.writeBlob(Buffer.from("v1 file content"));
    const treeId = repo.writeTree([{ path: "main.ts", mode: "100644", id: blobId }]);

    const commitId = repo.commitAs(
      { name: "Committer Carol", email: "carol@example.com", time: 1710000000, timezoneOffset: 0 },
      { name: "Author Dave", email: "dave@example.com", time: 1710000000, timezoneOffset: 0 },
      "HEAD",
      "Commit main.ts",
      treeId,
      [],
    );

    const headCommit = repo.findCommit(commitId);
    const tree = repo.findTree(headCommit.treeId);
    const entry = tree.entryByPath("main.ts");
    expect(entry).toBeDefined();

    const commitMeta = entry?.commitMetadata;
    expect(commitMeta).toBeDefined();
    expect(commitMeta?.authorName).toBe("Author Dave");

    const manifest = repo.replaceObject(entry!.id, "entry", {
      path: "main.ts",
      content: Buffer.from("v2 file content"),
    });

    expect(manifest.initOid).toBe(entry!.id);
    expect(manifest.kind).toBe("entry");
    expect(manifest.path).toBe("main.ts");
    expect(manifest.history[0].authorName).toBe("Author Dave");
    expect(manifest.history[0].authorEmail).toBe("dave@example.com");
  });

  test("replaces the same object OID several times and merges history into refs/replace/{initOid}", () => {
    const { repo } = setupRepo();
    const initBlobId = repo.writeBlob(Buffer.from("v0 initial"));

    // First replacement
    const m1 = repo.replaceObject(initBlobId, "entry", {
      path: "README.md",
      content: Buffer.from("v1 replacement"),
    });
    const rep1 = m1.latestReplaceOid;

    // Second replacement
    const m2 = repo.replaceObject(initBlobId, "entry", {
      path: "README.md",
      content: Buffer.from("v2 replacement"),
    });
    const rep2 = m2.latestReplaceOid;

    // Third replacement
    const m3 = repo.replaceObject(initBlobId, "entry", {
      path: "README.md",
      content: Buffer.from("v3 replacement"),
    });
    const rep3 = m3.latestReplaceOid;

    expect(rep1).not.toBe(rep2);
    expect(rep2).not.toBe(rep3);

    // Initial OID ref remains stable canonical ref
    expect(m3.initOid).toBe(initBlobId);
    expect(m3.latestReplaceOid).toBe(rep3);
    expect(m3.history.length).toBe(3);
    expect(m3.history[0].replaceOid).toBe(rep1);
    expect(m3.history[1].replaceOid).toBe(rep2);
    expect(m3.history[2].replaceOid).toBe(rep3);

    const fetched = repo.getReplaceManifest(initBlobId);
    expect(fetched).toBeDefined();
    expect(fetched?.history.length).toBe(3);
    expect(fetched?.latestReplaceOid).toBe(rep3);
  });
});
