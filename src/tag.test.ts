import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { expect, test } from "vite-plus/test";
import { Repository, type Commit, type Tag } from "./main.ts";

/**
 * JS Examples & Tests for Tags, Commits, Objects, and References:
 *
 * 1. Creating an Annotated Tag (Tagged Object):
 *    ```ts
 *    const tagRef = repo.createTag("v1.0.0", commitId, "Release v1.0.0 message");
 *    const tagObj = repo.findTag(tagRef.target!);
 *    console.log(tagObj.name, tagObj.targetId, tagObj.targetKind, tagObj.message);
 *    ```
 *
 * 2. Creating a Lightweight Tag:
 *    ```ts
 *    const lightRef = repo.createLightweightTag("v1.0-lightweight", commitId);
 *    expect(lightRef.target).toBe(commitId);
 *    ```
 *
 * 3. Working with References:
 *    ```ts
 *    const ref = repo.createReference("refs/heads/feature", commitId);
 *    ref.delete();
 *    ```
 *
 * 4. Downcasting Git Objects:
 *    ```ts
 *    const obj = repo.findObject(commitId);
 *    const commit = obj.asCommit();
 *    ```
 */

function setupTempRepo(): {
  repo: Repository;
  commit: Commit;
  cleanup: () => void;
} {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "repo-test-tags-"));
  const repo = Repository.init(tmpDir, false);

  // Write a blob and tree to create a valid commit
  repo.writeBlob(Buffer.from("Hello Git tags\n"));
  const headCommitTree = Repository.open(process.cwd()).headCommit().treeId;

  const commitId = repo.commit(
    "refs/heads/main",
    "Initial commit for tag testing",
    headCommitTree,
    [],
  );
  const commit = repo.findCommit(commitId);

  const cleanup = () => {
    try {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    } catch {}
  };

  return { repo, commit, cleanup };
}

// --- 1. Annotated Tag Creation & Inspection -----------------------------------

test("createTag() creates an annotated tag pointing to a commit", () => {
  const { repo, commit, cleanup } = setupTempRepo();
  try {
    const tagName = "v1.0.0";
    const tagMessage = "Release version 1.0.0\n\nDetailed release notes here.";

    // Create an annotated tag
    const tagRef = repo.createTag(tagName, commit.id, tagMessage);

    expect(tagRef.name).toBe("refs/tags/v1.0.0");
    expect(typeof tagRef.target).toBe("string"); // The object ID of the created annotated tag

    // Find and decode the annotated tag object from ODB
    const tag: Tag = repo.findTag(tagRef.target!);

    expect(tag.name).toBe("v1.0.0");
    expect(tag.targetId).toBe(commit.id);
    expect(tag.targetKind).toBe("Commit");
    expect(tag.message).toBe(tagMessage);

    // Tagger signature should be populated if committer config is present
    if (tag.tagger) {
      expect(typeof tag.tagger.name).toBe("string");
      expect(typeof tag.tagger.time).toBe("number");
    }
  } finally {
    cleanup();
  }
});

// --- 2. Lightweight Tag Creation ---------------------------------------------

test("createLightweightTag() creates a direct tag reference without a tag object", () => {
  const { repo, commit, cleanup } = setupTempRepo();
  try {
    const tagName = "v1.0-lightweight";
    const ref = repo.createLightweightTag(tagName, commit.id);

    expect(ref.name).toBe("refs/tags/v1.0-lightweight");
    // Lightweight tag ref targets the commit directly
    expect(ref.target).toBe(commit.id);

    // Verify it exists in references
    const found = repo.findReference("refs/tags/v1.0-lightweight");
    expect(found.target).toBe(commit.id);
  } finally {
    cleanup();
  }
});

// --- 3. References Operations ------------------------------------------------

test("createReference(), findReference(), and delete() manage refs", () => {
  const { repo, commit, cleanup } = setupTempRepo();
  try {
    const refName = "refs/heads/feature/tag-support";
    const created = repo.createReference(refName, commit.id);

    expect(created.name).toBe(refName);
    expect(created.target).toBe(commit.id);

    // List all references in repo
    const allRefs = repo.references();
    expect(allRefs.some((r) => r.name === refName)).toBe(true);

    // Find single reference
    const ref = repo.findReference(refName);
    expect(ref.target).toBe(commit.id);

    // Delete the reference
    ref.delete();

    // Verify deletion
    expect(() => repo.findReference(refName)).toThrow();
  } finally {
    cleanup();
  }
});

// --- 4. Downcasting GitObjects ------------------------------------------------

test("findObject() supports downcasting to Commit, Tree, Tag, Blob", () => {
  const { repo, commit, cleanup } = setupTempRepo();
  try {
    // Find object as GitObject
    const gitObj = repo.findObject(commit.id);
    expect(gitObj.kind).toBe("commit");

    const asCommit = gitObj.asCommit();
    expect(asCommit).not.toBeNull();
    expect(asCommit!.id).toBe(commit.id);
    expect(asCommit!.message).toBe(commit.message);

    expect(gitObj.asTree()).toBeNull();
    expect(gitObj.asBlob()).toBeNull();
    expect(gitObj.asTag()).toBeNull();

    // Create an annotated tag and test downcasting to Tag
    const tagRef = repo.createTag("v2.0.0", commit.id, "Tag v2");
    const tagObj = repo.findObject(tagRef.target!);
    expect(tagObj.kind).toBe("tag");

    const asTag = tagObj.asTag();
    expect(asTag).not.toBeNull();
    expect(asTag!.name).toBe("v2.0.0");
    expect(asTag!.targetId).toBe(commit.id);
  } finally {
    cleanup();
  }
});
