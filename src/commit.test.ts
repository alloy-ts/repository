import { expect, test } from "vite-plus/test";
import { Repository } from "../dist/index.js";
import * as fs from "node:fs";
import * as path from "node:path";
import * as os from "node:os";

function createTmpDir(prefix: string): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

function removeTmpDir(dirPath: string): void {
  fs.rmSync(dirPath, { recursive: true, force: true });
}

test("commit creation, reading signatures, and verification structure", () => {
  const tmpDir = createTmpDir("git-commit-test-");

  try {
    const repo = Repository.init(tmpDir, false);

    // 1. Create a blob
    const blobBuffer = Buffer.from("hello commit test\n");
    const blobId = repo.writeBlob(blobBuffer);
    expect(typeof blobId).toBe("string");
    expect(blobId.length).toBe(40);

    // 2. Read back blob content
    const blob = repo.findBlob(blobId);
    expect(blob.id).toBe(blobId);
    expect(blob.content().toString()).toBe("hello commit test\n");

    // 3. Create initial commit pointing to empty tree or tree ID if available
    // `repo.commit` creates a commit on a branch (e.g. `refs/heads/main`)
    const emptyTreeId = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    const commitId = repo.commit(
      "refs/heads/main",
      "Initial commit for testing",
      emptyTreeId,
      []
    );
    expect(typeof commitId).toBe("string");
    expect(commitId.length).toBe(40);

    // 4. Verify the created commit
    const commit = repo.findCommit(commitId);
    expect(commit.id).toBe(commitId);
    expect(commit.message).toBe("Initial commit for testing");
    expect(commit.treeId).toBe(emptyTreeId);
    expect(commit.parentIds).toEqual([]);

    // Check author and committer signatures
    const author = commit.author;
    expect(author).toBeDefined();
    expect(typeof author.name).toBe("string");
    expect(typeof author.email).toBe("string");
    expect(typeof author.time).toBe("number");

    const committer = commit.committer;
    expect(committer).toBeDefined();
    expect(typeof committer.name).toBe("string");
    expect(typeof committer.email).toBe("string");

    // 5. Create a child commit with parents
    const childCommitId = repo.commit(
      "refs/heads/main",
      "Second commit",
      emptyTreeId,
      [commitId]
    );
    const childCommit = repo.findCommit(childCommitId);
    expect(childCommit.parentIds).toEqual([commitId]);

    const parents = childCommit.parents();
    expect(parents.length).toBe(1);
    expect(parents[0].id).toBe(commitId);

    // 6. Verify HEAD points to second commit
    expect(repo.headId()).toBe(childCommitId);
  } finally {
    removeTmpDir(tmpDir);
  }
});
