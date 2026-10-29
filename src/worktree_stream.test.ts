import { execSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";

import { Repository } from "./main.ts";

let dir: string;
let repo: Repository;

beforeAll(() => {
  dir = mkdtempSync(join(tmpdir(), "repo-wtstream-"));
  // Build a small, known-content repo so the streamed entries are deterministic.
  execSync("git init -q -b main", { cwd: dir, stdio: "ignore" });
  writeFileSync(join(dir, "README.md"), "# hello\n");
  mkdirSync(join(dir, "src"), { recursive: true });
  writeFileSync(join(dir, "src", "app.ts"), "console.log(1)\n");
  mkdirSync(join(dir, "docs"), { recursive: true });
  writeFileSync(join(dir, "docs", "guide.md"), "guide\n");
  execSync("git -c user.email=t@t.t -c user.name=t add -A", {
    cwd: dir,
    stdio: "ignore",
  });
  execSync("git -c user.email=t@t.t -c user.name=t -c commit.gpgsign=false commit -q -m init", {
    cwd: dir,
    stdio: "ignore",
  });
  repo = Repository.open(dir);
});

afterAll(() => {
  rmSync(dir, { recursive: true, force: true });
});

describe("Repository.worktreeStream (src/worktree.rs)", () => {
  test("materialises the HEAD tree into typed entries", async () => {
    const treeId = repo.headCommit().treeId;
    const entries = await repo.worktreeStream(treeId);

    expect(Array.isArray(entries)).toBe(true);
    const paths = entries.map((e) => e.path);
    expect(paths).toContain("README.md");
    expect(paths).toContain("src/app.ts");
    expect(paths).toContain("docs/guide.md");

    for (const e of entries) {
      expect(typeof e.path).toBe("string");
      expect(typeof e.mode).toBe("string");
      expect(e.id).toMatch(/^[0-9a-f]{40}$/);
      expect(e.size === null || typeof e.size === "number").toBe(true);
    }
  });

  test("classifies files as blobs with the matching size", async () => {
    const treeId = repo.headCommit().treeId;
    const entries = await repo.worktreeStream(treeId);

    const readme = entries.find((e) => e.path === "README.md");
    expect(readme).toBeDefined();
    expect(readme!.mode).toContain("Blob");
    // No .gitattributes in this repo, so no smudge/ident filters rewrite the content.
    expect(readme!.size).toBe("# hello\n".length);

    const app = entries.find((e) => e.path === "src/app.ts");
    expect(app).toBeDefined();
    expect(app!.mode).toContain("Blob");
    expect(app!.size).toBe("console.log(1)\n".length);
  });

  test("emits directory entries as trees when present", async () => {
    const treeId = repo.headCommit().treeId;
    const entries = await repo.worktreeStream(treeId);
    const src = entries.find((e) => e.path === "src");
    // gix's stream may include directory (tree) entries; if so they are typed as Tree.
    if (src) {
      expect(src.mode).toContain("Tree");
    }
  });

  test("rejects an unparseable tree id", async () => {
    await expect(repo.worktreeStream("not-a-real-id")).rejects.toThrow();
  });

  test("rejects a well-formed but missing object id", async () => {
    await expect(repo.worktreeStream("0".repeat(40))).rejects.toThrow();
  });
});
