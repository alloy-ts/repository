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
});
