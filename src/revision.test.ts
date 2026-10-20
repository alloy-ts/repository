import { execSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-revision-"));
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
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true });
});

describe("Repository revision specs (src/revision.rs)", () => {
  test("revParseSingle / revParse / revWalk", () => {
    const tip = repo.revParseSingle("HEAD");
    expect(tip).toBe(repo.headId());
    // revParse returns a Spec object (not a bare id).
    expect(repo.revParse("HEAD").id).toBe(repo.revParseSingle("HEAD"));

    const walked = repo.revWalk([tip]);
    expect(walked[0]).toBe(tip);
    expect(walked.length).toBeGreaterThanOrEqual(1);
  });

  test("revParseHex() resolves HEAD to the head id", () => {
    expect(repo.revParseHex("HEAD")).toBe(repo.headId());
  });

  test("revParse() returns a Spec with kind and id", () => {
    const spec = repo.revParse("HEAD");
    expect(spec.kind).toBe("single");
    expect(spec.id).toBe(repo.headId());
  });

  test("revParse() distinguishes ranges and exclusions", () => {
    const exclude = repo.revParse("^HEAD");
    expect(exclude.kind).toBe("exclude");
    expect(exclude.id).toBe(repo.headId());

    // A range needs at least one parent commit to resolve `HEAD~1`.
    if (repo.headCommit().parentIds.length > 0) {
      const range = repo.revParse("HEAD~1..HEAD");
      expect(range.kind).toBe("range");
      expect(range.to).toBe(repo.headId());
      expect(typeof range.from).toBe("string");
    }
  });

  test("revWalk() accepts sort/reverse/firstParent options", () => {
    const head = repo.revParseSingle("HEAD");
    const def = repo.revWalk([head]);
    const asc = repo.revWalk([head], { sort: "time-oldest" });
    const reversed = repo.revWalk([head], { reverse: true });
    const firstParent = repo.revWalk([head], { firstParent: true });
    expect(asc.length).toBe(def.length);
    expect(reversed.length).toBe(def.length);
    expect(firstParent.length).toBeGreaterThanOrEqual(1);
    expect(firstParent.length).toBeLessThanOrEqual(def.length);
  });

  test("get file (e.g. ReadMe.md or repository.manifest.yml) from a specific revision or SHA", () => {
    // 1. Resolve revision spec (e.g. "HEAD" or SHA)
    const commitId = repo.revParseSingle("HEAD");
    const commit = repo.findCommit(commitId);

    // 2. Inspect tree and retrieve file entry
    const tree = repo.findTree(commit.treeId);
    const entry = tree.entry("ReadMe.md") ?? tree.entry("repository.manifest.yml");

    expect(entry).not.toBeNull();
    if (entry) {
      // 3. Find blob by OID and read content
      const blob = repo.findBlob(entry.id);
      const content = blob.content().toString("utf-8");
      expect(content.length).toBeGreaterThan(0);
    }
  });

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
});
