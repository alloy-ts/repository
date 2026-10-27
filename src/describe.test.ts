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
  base = mkdtempSync(join(tmpdir(), "repo-describe-"));
  cloneDir = join(base, "clone");
  try {
    repo = await Repository.clone(realRoot, cloneDir);
  } catch {
    rmSync(cloneDir, { recursive: true, force: true });
    execSync(`git clone --local --quiet ${JSON.stringify(realRoot)} ${JSON.stringify(cloneDir)}`, {
      stdio: "ignore",
    });
    repo = Repository.open(cloneDir);
  }
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true });
});

describe("Repository / Object describe (src/describe.rs)", () => {
  test("describe() reports exact tag matches", () => {
    execSync(
      `git -c user.email=t@example.com -c user.name=T -c commit.gpgsign=false -c tag.gpgsign=false tag -a describe-test-v1 -m "tag" HEAD`,
      {
        cwd: cloneDir,
        stdio: "ignore",
      },
    );
    const head = repo.headId();
    const exact = repo.describe(head, { names: "allTags" });
    expect(exact).not.toBeNull();
    expect(exact!.name).toBe("describe-test-v1");
    expect(exact!.id).toBe(head);
    expect(exact!.depth).toBe(0);
    expect(exact!.exactMatch).toBe(true);
    expect(exact!.format).toBe("describe-test-v1");
  });

  test("describe() counts depth for commits ahead of the closest tag", () => {
    execSync(
      `git -c user.email=t@example.com -c user.name=T -c commit.gpgsign=false -c tag.gpgsign=false commit --allow-empty -m "describe ahead"`,
      { cwd: cloneDir, stdio: "ignore" },
    );
    const head = repo.headId();
    const ahead = repo.describe(head, { names: "allTags" });
    expect(ahead).not.toBeNull();
    expect(ahead!.name).toBe("describe-test-v1");
    expect(ahead!.depth).toBe(1);
    expect(ahead!.exactMatch).toBe(false);
    expect(ahead!.format).toMatch(/^describe-test-v1-1-g[0-9a-f]{7,}$/);
    expect(ahead!.commitsSeen).toBeGreaterThan(0);
  });

  test("describe() falls back to the id, or returns null, when no candidate is used", () => {
    const head = repo.headId();
    const fallback = repo.describe(head, {
      maxCandidates: 0,
      idAsFallback: true,
    });
    expect(fallback).not.toBeNull();
    expect(fallback!.name == null).toBe(true);
    expect(fallback!.id).toBe(head);
    expect(fallback!.format).toMatch(/^[0-9a-f]+$/);
    expect(repo.describe(head, { maxCandidates: 0 })).toBeNull();
  });

  test("describe() delegates to Repository.describe and rejects non-commits", () => {
    const commit = repo.headCommit();
    const outcome = repo.findObject(commit.id).describe({ idAsFallback: true });
    expect(outcome?.id).toBe(commit.id);
    expect(() => repo.findObject(commit.treeId).describe()).toThrow();
  });

  test("describe() appends dirtySuffix only when the worktree is dirty", () => {
    execSync(
      `git -c user.email=t@example.com -c user.name=T -c commit.gpgsign=false -c tag.gpgsign=false tag -a describe-dirty-v1 -m "tag" HEAD`,
      { cwd: cloneDir, stdio: "ignore" },
    );
    const head = repo.headId();
    const clean = repo.describe(head, {
      names: "allTags",
      dirtySuffix: "-dirty",
    });
    expect(clean).not.toBeNull();
    expect(clean!.format).toBe("describe-dirty-v1");

    writeFileSync(join(cloneDir, "untracked-dirty.txt"), "dirty\n");
    const dirty = repo.describe(head, {
      names: "allTags",
      dirtySuffix: "-dirty",
    });
    expect(dirty!.format).toBe("describe-dirty-v1-dirty");
  });
});
