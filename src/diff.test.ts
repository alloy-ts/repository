import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

function git(args: string, cwd: string) {
  execSync(`git ${args}`, { cwd, stdio: "ignore" });
}

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-diff-"));
  cloneDir = join(base, "clone");
  try {
    repo = await Repository.clone(realRoot, cloneDir, false);
  } catch (e) {
    rmSync(cloneDir, { recursive: true, force: true });
    execSync(`git clone --local --quiet ${JSON.stringify(realRoot)} ${JSON.stringify(cloneDir)}`, {
      stdio: "ignore",
    });
    repo = Repository.open(cloneDir);
  }
  writeFileSync(join(cloneDir, "diff_test.txt"), "hello diff\n");
  git("add -A", cloneDir);
  git("config commit.gpgsign false", cloneDir);
  git('-c user.email=test@example.com -c user.name=Test commit -m "diff test commit"', cloneDir);
  repo = Repository.open(cloneDir);
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true });
});

describe("diffTreeToTree bindings in src/diff.rs", () => {
  test("diffTreeToTree diffs tree against null (empty tree)", () => {
    const headCommit = repo.findCommit(repo.headId());
    const changes = repo.diffTreeToTree(null, headCommit.treeId);
    expect(Array.isArray(changes)).toBe(true);
    expect(changes.length).toBeGreaterThan(0);
    const found = changes.find((c) => c.path === "diff_test.txt");
    expect(found).toBeDefined();
    expect(found?.kind).toBe("Added");
  });
});
