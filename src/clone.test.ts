import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Cloner, Repository } from "./main.ts";

let base: string;
let origin: string; // bare origin path
let seed: string;
const dirs: string[] = [];

function sh(command: string, cwd = process.cwd()): string {
  return execSync(command, { cwd }).toString();
}
function git(args: string, cwd = process.cwd()) {
  execSync(`git ${args}`, { cwd, stdio: "ignore" });
}
function commit(name: string, content: string, message: string, cwd: string): string {
  writeFileSync(join(cwd, name), content);
  git("add -A", cwd);
  git(`commit -m ${JSON.stringify(message)}`, cwd);
  return sh("git rev-parse HEAD", cwd).trim();
}

beforeAll(() => {
  base = mkdtempSync(join(tmpdir(), "clone-test-"));
  dirs.push(base);
  origin = join(base, "origin.git");
  git(`init --bare ${JSON.stringify(origin)}`);
  seed = mkdtempSync(join(tmpdir(), "clone-seed-"));
  dirs.push(seed);
  git(`init ${JSON.stringify(seed)}`);
  commit("a.txt", "hello", "init", seed);
  git(`remote add origin ${JSON.stringify(origin)}`, seed);
  git("push -u origin HEAD", seed);
});

afterAll(() => {
  for (const d of dirs) rmSync(d, { recursive: true, force: true });
});

describe("Cloner", () => {
  test("isValidGitUrl validates schemes", () => {
    expect(Cloner.isValidGitUrl("https://github.com/user/repo.git")).toBe(true);
    expect(Cloner.isValidGitUrl("git@github.com:user/repo.git")).toBe(true);
    expect(Cloner.isValidGitUrl("ssh://git@github.com/user/repo.git")).toBe(true);
    expect(Cloner.isValidGitUrl("file:///path/to/repo.git")).toBe(true);
    expect(Cloner.isValidGitUrl("not-a-url")).toBe(false);
  });

  test("extractRepoName derives a sanitized name", () => {
    expect(Cloner.extractRepoName("https://github.com/user/repo.git")).toBe("repo");
    expect(Cloner.extractRepoName("git@github.com:user/repo.git")).toBe("repo");
    expect(Cloner.extractRepoName("ssh://git@example.com/path/to/repo.git")).toBe("repo");
    expect(() => Cloner.extractRepoName("not-a-url")).toThrow();
  });

  test("convertSshToHttps transforms SSH URLs", () => {
    expect(Cloner.convertSshToHttps("git@github.com:user/repo.git")).toBe(
      "https://github.com/user/repo.git",
    );
    expect(Cloner.convertSshToHttps("ssh://git@github.com/user/repo.git")).toBe(
      "https://github.com/user/repo.git",
    );
    expect(Cloner.convertSshToHttps("https://github.com/user/repo.git")).toBeNull();
  });

  test("clone() with explicit path clones the repository", async () => {
    const dest = mkdtempSync(join(tmpdir(), "clone-dest-"));
    dirs.push(dest);
    const cloner = new Cloner();
    const repo = await cloner.clone(origin, dest);
    expect(repo).toBeInstanceOf(Repository);
    const head = sh("git rev-parse HEAD", dest).trim();
    const originHead = sh(`git --git-dir=${origin} rev-parse HEAD`).trim();
    expect(head).toBe(originHead);
  });

  test("clone() rejects an existing destination", async () => {
    const dest = mkdtempSync(join(tmpdir(), "clone-exists-"));
    dirs.push(dest);
    const cloner = new Cloner();
    await expect(cloner.clone(origin, dest)).rejects.toThrow();
  });

  test("clone() auto-derives the destination name when path is omitted", async () => {
    // `origin` ends in `origin.git` → derived name is `origin`; clone beside the bare repo.
    const cloner = new Cloner();
    const repo = await cloner.clone(origin);
    expect(repo).toBeInstanceOf(Repository);
    dirs.push(join(process.cwd(), "origin"));
  });
});

describe("Repository.clone", () => {
  test("clone() with explicit path returns a Repository", async () => {
    const dest = mkdtempSync(join(tmpdir(), "repo-clone-"));
    dirs.push(dest);
    const repo = await Repository.clone(origin, dest);
    expect(repo).toBeInstanceOf(Repository);
    const head = sh("git rev-parse HEAD", dest).trim();
    const originHead = sh(`git --git-dir=${origin} rev-parse HEAD`).trim();
    expect(head).toBe(originHead);
  });

  test("clone() with no path auto-derives the destination name", async () => {
    // Scope the auto-derived "origin" directory to a scratch dir so it cannot collide
    // with the Cloner auto-derive test above.
    const prev = process.cwd();
    const scratch = mkdtempSync(join(tmpdir(), "repo-autoderive-"));
    dirs.push(scratch);
    process.chdir(scratch);
    try {
      const repo = await Repository.clone(origin);
      expect(repo).toBeInstanceOf(Repository);
      const head = sh("git rev-parse HEAD", join(scratch, "origin")).trim();
      const originHead = sh(`git --git-dir=${origin} rev-parse HEAD`).trim();
      expect(head).toBe(originHead);
    } finally {
      process.chdir(prev);
    }
  });

  test("clone() rejects an existing destination", async () => {
    const dest = mkdtempSync(join(tmpdir(), "repo-clone-exists-"));
    dirs.push(dest);
    await expect(Repository.clone(origin, dest)).rejects.toThrow();
  });
});

describe("Repository clone helpers (static)", () => {
  test("Repository.isValidGitUrl validates schemes", () => {
    expect(Repository.isValidGitUrl("https://github.com/user/repo.git")).toBe(true);
    expect(Repository.isValidGitUrl("git@github.com:user/repo.git")).toBe(true);
    expect(Repository.isValidGitUrl("file:///path/to/repo.git")).toBe(true);
    expect(Repository.isValidGitUrl("not-a-url")).toBe(false);
  });

  test("Repository.extractRepoName derives a sanitized name", () => {
    expect(Repository.extractRepoName("https://github.com/user/repo.git")).toBe("repo");
    expect(Repository.extractRepoName("git@github.com:user/repo.git")).toBe("repo");
    expect(() => Repository.extractRepoName("not-a-url")).toThrow();
  });

  test("Repository.convertSshToHttps transforms SSH URLs", () => {
    expect(Repository.convertSshToHttps("git@github.com:user/repo.git")).toBe(
      "https://github.com/user/repo.git",
    );
    expect(Repository.convertSshToHttps("https://github.com/user/repo.git")).toBeNull();
  });
});
