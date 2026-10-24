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

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-task-"));
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
  execSync("git config user.name 'Test User'", { cwd: cloneDir });
  execSync("git config user.email 'test@example.com'", { cwd: cloneDir });
});

afterAll(() => {
  rmSync(base, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 200,
  });
});

describe("Repository task() operations", () => {
  test("repo.task() returns a TaskOps handle", () => {
    const taskOps = repo.task();
    expect(taskOps).toBeDefined();
    expect(typeof taskOps.featureBranch).toBe("function");
    expect(typeof taskOps.commitAll).toBe("function");
    expect(typeof taskOps.sync).toBe("function");
    expect(typeof taskOps.squashMerge).toBe("function");
  });

  test("featureBranch creates a new branch and switches to it", async () => {
    const head = repo.headId();
    await repo.task().featureBranch("feature/test-widget", head);

    const branches = repo.branchNames();
    expect(branches).toContain("feature/test-widget");

    // Switching again to an existing branch throws an error
    await expect(repo.task().featureBranch("feature/test-widget", head)).rejects.toThrow();

    // Clean up: switch back to default branch and delete test branch
    execSync("git checkout -", { cwd: cloneDir });
    repo.deleteLocalBranches(["feature/test-widget"]);
  });

  test("commitAll stages dirty changes and commits them", async () => {
    const filePath = join(cloneDir, "task_test_file.txt");
    writeFileSync(filePath, "task ops test content");

    await repo.task().commitAll("wip: task ops commit test");

    const log = repo.log(undefined, 1, undefined);
    expect(log[0].message).toContain("wip: task ops commit test");
  });

  test("squashMerge squashes changes from target branch into current branch without committing", async () => {
    // Create side branch with a commit
    const head = repo.headId();
    await repo.task().featureBranch("feature/squash-source", head);

    const filePath = join(cloneDir, "squash_file.txt");
    writeFileSync(filePath, "squash content");
    await repo.task().commitAll("commit to squash");

    // Switch back to main / previous branch
    execSync("git checkout -", { cwd: cloneDir });

    // Squash merge
    await repo.task().squashMerge("feature/squash-source");

    // Verify changes are staged
    const status = await repo.status();
    expect(status.some((s) => s.path === "squash_file.txt")).toBe(true);

    // Clean up: reset working directory and delete branch
    execSync("git reset --hard", { cwd: cloneDir });
    repo.deleteLocalBranches(["feature/squash-source"]);
  });
});
