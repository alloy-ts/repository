import { describe, expect, test } from "vite-plus/test";
import type { RefInfo, Replace, ReplaceManifest } from "./main.ts";
import { Repository } from "./main.ts";

describe("Replace / ReplaceManifest (src/replace.rs)", () => {
  test("Replace structure handles fields", () => {
    const ref: RefInfo = {
      name: "refs/replace/1111111111111111111111111111111111111111",
      target: "2222222222222222222222222222222222222222",
    };

    const node: Replace = {
      this: ref,
      prev: undefined,
      next: undefined,
      head: ref,
      authorName: "Alice",
      authorEmail: "alice@example.com",
      updatedAt: "2026-10-20T12:00:00Z",
    };

    expect(node.this.name).toBe("refs/replace/1111111111111111111111111111111111111111");
    expect(node.authorName).toBe("Alice");
    expect(node.authorEmail).toBe("alice@example.com");
    expect(node.updatedAt).toBe("2026-10-20T12:00:00Z");
  });

  test("ReplaceManifest structure holds revisions array", () => {
    const ref: RefInfo = {
      name: "refs/replace/1111111111111111111111111111111111111111",
      target: "2222222222222222222222222222222222222222",
    };

    const node: Replace = {
      this: ref,
      prev: undefined,
      next: undefined,
      head: ref,
      authorName: "Bob",
      authorEmail: "bob@example.com",
      updatedAt: "2026-10-20T12:00:00Z",
    };

    const manifest: ReplaceManifest = {
      id: "1111111111111111111111111111111111111111",
      type: "blob",
      revisions: [node],
      head: ref,
      path: undefined,
    };

    expect(manifest.id).toBe("1111111111111111111111111111111111111111");
    expect(manifest.type).toBe("blob");
    expect(manifest.revisions.length).toBe(1);
    expect(manifest.revisions[0].authorName).toBe("Bob");
  });

  test("getReplaceNode returns null when replace ref does not exist", () => {
    const repo = Repository.init("/tmp/replace-test-repo-" + Date.now());
    const node = repo.getReplaceNode("1111111111111111111111111111111111111111");
    expect(node).toBeNull();
  });

  test("getReplaceManifest returns null when replace ref does not exist", () => {
    const repo = Repository.init("/tmp/replace-manifest-test-repo-" + Date.now());
    const manifest = repo.getReplaceManifest("1111111111111111111111111111111111111111");
    expect(manifest).toBeNull();
  });
});

describe("Replace for each git object type (blob, tree, commit, tag, entry)", () => {
  test("resolves replace manifest for blob, tree, commit, tag, and entry", () => {
    const repo = Repository.init("/tmp/replace-obj-types-" + Date.now());

    // 1. Create a blob
    const blob1 = repo.writeBlob(Buffer.from("hello content\n"));
    const blob2 = repo.writeBlob(Buffer.from("replacement content\n"));

    // 2. Create a tree containing the blob
    const tree1 = repo.writeTree([{ path: "file.txt", mode: "100644", id: blob1 }]);
    const tree2 = repo.writeTree([{ path: "file.txt", mode: "100644", id: blob2 }]);

    // 3. Create a commit
    const commit1 = repo.commit("refs/heads/main", "Initial commit", tree1, []);
    const commit2 = repo.newCommit("Replacement commit", tree2, []);

    // 4. Create an annotated tag object directly
    const tagData = Buffer.from(
      `object ${commit1}\ntype commit\ntag v1.0.0\ntagger Alice <alice@example.com> 1700000000 +0000\n\nRelease tag\n`
    );
    const tagObjId = repo.writeObject("tag", tagData);

    // Create replace references at refs/replace/<id> -> replacement_id
    repo.createReference(`refs/replace/${blob1}`, blob2);
    repo.createReference(`refs/replace/${tree1}`, tree2);
    repo.createReference(`refs/replace/${commit1}`, commit2);
    repo.createReference(`refs/replace/${tagObjId}`, commit2);

    // Test blob replace manifest
    const blobManifest = repo.getReplaceManifest(blob1);
    expect(blobManifest).not.toBeNull();
    expect(blobManifest!.type).toBe("blob");
    expect(blobManifest!.revisions[0].this.target).toBe(blob2);

    // Test tree replace manifest
    const treeManifest = repo.getReplaceManifest(tree1);
    expect(treeManifest).not.toBeNull();
    expect(treeManifest!.type).toBe("tree");
    expect(treeManifest!.revisions[0].this.target).toBe(tree2);

    // Test commit replace manifest
    const commitManifest = repo.getReplaceManifest(commit1);
    expect(commitManifest).not.toBeNull();
    expect(commitManifest!.type).toBe("commit");
    expect(commitManifest!.revisions[0].this.target).toBe(commit2);
    expect(commitManifest!.revisions[0].authorName).toBeDefined();

    // Test tag replace manifest
    const tagManifest = repo.getReplaceManifest(tagObjId);
    expect(tagManifest).not.toBeNull();
    expect(tagManifest!.type).toBe("tag");

    // Test entry replace manifest (with path parameter)
    const entryManifest = repo.getReplaceManifest(blob1, "file.txt");
    expect(entryManifest).not.toBeNull();
    expect(entryManifest!.type).toBe("entry");
    expect(entryManifest!.path).toBe("file.txt");
    expect(entryManifest!.revisions[0].authorName).toBeDefined();
    expect(entryManifest!.revisions[0].committerName).toBeDefined();
    expect(entryManifest!.revisions[0].commitTimestamp).toBeGreaterThan(0);
  });
});
