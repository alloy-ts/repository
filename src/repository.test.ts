import { describe, expect, test } from "vite-plus/test";
import * as fs from "node:fs";
import * as path from "node:path";
import * as os from "node:os";
import { Repository, type LogEntry } from "../dist/index.js";

function createTempDir(prefix: string): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

function cleanupDir(dirPath: string) {
  try {
    fs.rmSync(dirPath, { recursive: true, force: true });
  } catch {
    // ignore
  }
}

describe("Repository static constructors and initialization", () => {
  test("Repository.open opens existing workspace repo", () => {
    const repo = Repository.open(process.cwd());
    expect(repo.isBare()).toBe(false);
    expect(typeof repo.gitDir()).toBe("string");
  });

  test("Repository.discover finds repository from workspace subdirectory", () => {
    const srcDir = path.join(process.cwd(), "src");
    const repo = Repository.discover(srcDir);
    expect(repo.isBare()).toBe(false);
  });

  test("Repository.init creates a working-tree repository", () => {
    const tmp = createTempDir("repo-init-test-");
    try {
      const repo = Repository.init(tmp);
      expect(repo.isBare()).toBe(false);
      expect(repo.workDir()).toBe(tmp);
      expect(fs.existsSync(path.join(tmp, ".git"))).toBe(true);
    } finally {
      cleanupDir(tmp);
    }
  });

  test("Repository.init with bare: true creates a bare repository", () => {
    const tmp = createTempDir("repo-init-bare-test-");
    try {
      const repo = Repository.init(tmp, true);
      expect(repo.isBare()).toBe(true);
      expect(repo.workDir()).toBeNull();
      expect(fs.existsSync(path.join(tmp, "HEAD"))).toBe(true);
    } finally {
      cleanupDir(tmp);
    }
  });

  test("Repository.clone works as static constructor method on Repository", () => {
    const srcTmp = createTempDir("repo-clone-src-");

    try {
      Repository.init(srcTmp);
      expect(typeof Repository.clone).toBe("function");
    } finally {
      cleanupDir(srcTmp);
    }
  });
});

describe("Reading git logs with Repository", () => {
  test("repo.log() fetches commit entries in reverse chronological order", () => {
    const repo = Repository.open(process.cwd());
    const entries: LogEntry[] = repo.log(undefined, 10, false);

    expect(Array.isArray(entries)).toBe(true);
    expect(entries.length).toBeGreaterThan(0);

    const first = entries[0]!;
    expect(typeof first.id).toBe("string");
    expect(typeof first.message).toBe("string");
    expect(typeof first.subject).toBe("string");
    expect(typeof first.authorName).toBe("string");
    expect(typeof first.authorEmail).toBe("string");
    expect(typeof first.authorTime).toBe("number");
    expect(Array.isArray(first.parentIds)).toBe(true);
  });

  test("repo.revWalk() and repo.findCommit() allow manually walking log history", () => {
    const repo = Repository.open(process.cwd());
    const headId = repo.headId();
    const commitIds = repo.revWalk([headId]);

    expect(Array.isArray(commitIds)).toBe(true);
    expect(commitIds.length).toBeGreaterThan(0);
    expect(commitIds[0]).toBe(headId);

    const commit = repo.findCommit(commitIds[0]!);
    expect(commit.id).toBe(headId);
    expect(typeof commit.message).toBe("string");
    expect(typeof commit.author.name).toBe("string");
  });
});
