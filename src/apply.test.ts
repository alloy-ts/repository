import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;
let baseTree: string;

// A unified diff that: modifies a.txt (world -> earth), adds b.txt, deletes c.txt.
const PATCH = `diff --git a/a.txt b/a.txt
index 1111111111111111111111111111111111111111..2222222222222222222222222222222222222222 100644
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,2 @@
 hello
-world
+earth
diff --git a/b.txt b/b.txt
new file mode 100644
--- /dev/null
+++ b/b.txt
@@ -0,0 +1,1 @@
+new file
diff --git a/c.txt b/c.txt
deleted file mode 100644
--- a/c.txt
+++ /dev/null
@@ -1,1 +0,0 @@
-delete me
`;

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-apply-"));
  cloneDir = join(base, "clone");
  repo = await Repository.clone(realRoot, cloneDir);

  const a = repo.writeBlob(Buffer.from("hello\nworld\n"));
  const c = repo.writeBlob(Buffer.from("delete me\n"));
  baseTree = repo.writeTree([
    { path: "a.txt", id: a, mode: "100644" },
    { path: "c.txt", id: c, mode: "100644" },
  ]);
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true });
});

/** Read a blob's text by id. */
function text(id: string): string {
  return repo.findBlob(id).content().toString("utf8");
}

describe("Repository apply (src/apply.rs)", () => {
  test("applyToTree modifies, adds, and deletes files", () => {
    const newTree = repo.applyToTree(PATCH, baseTree);
    const tree = repo.findTree(newTree);
    const byName = new Map(tree.entries.map((e) => [e.filename, e.id]));

    // a.txt modified: world -> earth
    expect(byName.has("a.txt")).toBe(true);
    expect(text(byName.get("a.txt")!)).toBe("hello\nearth\n");

    // b.txt added
    expect(byName.has("b.txt")).toBe(true);
    expect(text(byName.get("b.txt")!)).toBe("new file\n");

    // c.txt deleted
    expect(byName.has("c.txt")).toBe(false);
  });

  test("applyToTree with options.check still returns the tree", () => {
    const withCheck = repo.applyToTree(PATCH, baseTree, { check: true });
    const without = repo.applyToTree(PATCH, baseTree);
    // Same content -> same tree id.
    expect(withCheck).toBe(without);
  });

  test("applyToTreeAsync agrees with the sync variant", async () => {
    const sync = repo.applyToTree(PATCH, baseTree);
    const async = await repo.applyToTreeAsync(PATCH, baseTree);
    expect(async).toBe(sync);
  });

  test("apply() is not yet implemented (no gix apply engine)", () => {
    expect(() => repo.apply(PATCH)).toThrow(/not yet implemented/i);
  });
});
