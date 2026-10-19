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

test("references, peeling, reflogs, and deletion", () => {
  const tmpDir = createTmpDir("git-ref-test-");

  try {
    const repo = Repository.init(tmpDir, false);
    const emptyTreeId = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

    // 1. Create a commit to generate a reflog entry
    const c1 = repo.commit("refs/heads/main", "feat: first commit", emptyTreeId, []);
    const c2 = repo.commit("refs/heads/main", "fix: second commit", emptyTreeId, [c1]);

    // 2. Find reference
    const mainRef = repo.findReference("refs/heads/main");
    expect(mainRef.name).toBe("refs/heads/main");
    expect(mainRef.target).toBe(c2);

    // 3. Peel reference
    expect(mainRef.peel()).toBe(c2);

    // 4. Reflog inspection
    const logs = mainRef.reflog();
    expect(Array.isArray(logs)).toBe(true);
    expect(logs.length).toBeGreaterThan(0);
    expect(logs[0].newId).toBe(c1);

    // 5. Create custom reference and delete it
    const featureRef = repo.createReference("refs/heads/feature", c1, false);
    expect(featureRef.name).toBe("refs/heads/feature");
    expect(featureRef.target).toBe(c1);

    const allRefsBefore = repo.references().map((r) => r.name);
    expect(allRefsBefore).toContain("refs/heads/feature");

    featureRef.delete();

    const allRefsAfter = repo.references().map((r) => r.name);
    expect(allRefsAfter).not.toContain("refs/heads/feature");
  } finally {
    removeTmpDir(tmpDir);
  }
});
