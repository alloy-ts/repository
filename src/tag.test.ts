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

test("annotated and lightweight tags, commits, and refs", () => {
  const tmpDir = createTmpDir("git-tag-test-");

  try {
    const repo = Repository.init(tmpDir, false);
    const emptyTreeId = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

    // 1. Create a commit
    const commitId = repo.commit("refs/heads/main", "feat: initial commit", emptyTreeId, []);
    expect(typeof commitId).toBe("string");

    // 2. Create an annotated tag pointing to the commit
    const tagRef = repo.createTag("v1.0.0", commitId, "Release version 1.0.0", false);
    expect(tagRef.name).toBe("refs/tags/v1.0.0");
    expect(tagRef.target).toBeDefined();

    // Peel the tag reference to get the underlying commit ID
    const peeledId = tagRef.peel();
    expect(peeledId).toBe(commitId);

    // 3. Find and verify the created Tag object
    const tagObject = repo.findTag(tagRef.target!);
    expect(tagObject.name).toBe("v1.0.0");
    expect(tagObject.targetId).toBe(commitId);
    expect(tagObject.message).toBe("Release version 1.0.0");

    // 4. Create a lightweight tag
    const lwRef = repo.createLightweightTag("v1.0.0-lightweight", commitId, false);
    expect(lwRef.name).toBe("refs/tags/v1.0.0-lightweight");
    expect(lwRef.target).toBe(commitId);
    expect(lwRef.peel()).toBe(commitId);

    // 5. Verify references list contains created tags
    const refs = repo.references();
    const refNames = refs.map((r) => r.name);
    expect(refNames).toContain("refs/tags/v1.0.0");
    expect(refNames).toContain("refs/tags/v1.0.0-lightweight");
  } finally {
    removeTmpDir(tmpDir);
  }
});
