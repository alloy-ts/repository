import { execSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { FilterPipeline, Repository } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;
let pipeline: FilterPipeline;

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-filter-"));
  cloneDir = join(base, "clone");
  try {
    repo = await Repository.clone(realRoot, cloneDir, false);
  } catch {
    rmSync(cloneDir, { recursive: true, force: true });
    execSync(`git clone --local --quiet ${JSON.stringify(realRoot)} ${JSON.stringify(cloneDir)}`, {
      stdio: "ignore",
    });
    repo = Repository.open(cloneDir);
  }
  pipeline = new FilterPipeline(repo);
});

afterAll(() => {
  rmSync(base, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 200,
  });
});

describe("Repository filter pipeline (src/filter.rs)", () => {
  test("convertToGit / convertToWorktree return buffers and round-trip plain bytes", () => {
    const src = Buffer.from("hello\n");
    const toGit = pipeline.convertToGit(src, "data.txt");
    expect(Buffer.isBuffer(toGit)).toBe(true);
    // With no attributes/CRLF config, plain bytes pass through unchanged.
    expect(Buffer.from(toGit)).toEqual(src);

    const toWork = pipeline.convertToWorktree(src, "data.txt");
    expect(Buffer.isBuffer(toWork)).toBe(true);
    expect(Buffer.from(toWork)).toEqual(src);
  });

  test("worktreeFileToObject() returns null for a missing path", () => {
    expect(pipeline.worktreeFileToObject("does-not-exist.txt")).toBe(null);
  });

  test("worktreeFileToObject() records a real file with an id and kind", () => {
    const result = pipeline.worktreeFileToObject("README.md");
    expect(result).not.toBe(null);
    expect(result!.id).toMatch(/^[0-9a-f]{40}$/);
    expect(result!.isFile).toBe(true);
    expect(["Blob", "BlobExecutable"]).toContain(result!.kind);
  });
});
