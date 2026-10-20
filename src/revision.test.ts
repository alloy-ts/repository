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

  test("revWalk() accepts revision specs, excludes, globs and ranges", () => {
    // Specs, not just object ids.
    expect(repo.revWalk(["HEAD"])).toEqual(repo.revWalk([repo.headId()]));
    expect(repo.revWalk(["HEAD~1"])[0]).toBe(repo.revParseSingle("HEAD~1"));

    // exclude: hiding the tip empties the walk; hiding a parent keeps later commits.
    expect(repo.revWalk(["HEAD"], { exclude: ["HEAD"] })).toEqual([]);
    const head = repo.headCommit();
    const parent = head.parentIds[0];
    if (parent) {
      const viaExclude = repo.revWalk(["HEAD"], { exclude: [parent] });
      const viaRange = repo.revWalk([], { range: `${parent}..HEAD` });
      expect(viaRange).toEqual(viaExclude);
      expect(viaRange).not.toContain(parent);
    }

    // glob: implied refs/ prefix and /* suffix follow git's push_glob rules.
    execSync("git tag probe-glob-tag HEAD", { cwd: cloneDir, stdio: "ignore" });
    const globTips = repo.revWalk([], { glob: "refs/tags/*" });
    expect(globTips.length).toBeGreaterThan(0);
    expect(repo.revWalk([], { glob: "tags/*" })).toEqual(globTips);
    expect(repo.revWalk([], { glob: "refs/tags" })).toEqual(globTips);

    // Ranges and globs compose with the other options.
    const sorted = repo.revWalk([], {
      glob: "refs/tags/*",
      sort: "time-newest",
      reverse: true,
    });
    expect(sorted.slice().sort()).toEqual(globTips.slice().sort());
    execSync("git tag -d probe-glob-tag", { cwd: cloneDir, stdio: "ignore" });

    // symmetric range: commits reachable from either side but not from both.
    if (parent) {
      const sym = repo.revWalk([], { range: `${parent}...HEAD` });
      // Both sides pushed, merge base hidden: HEAD's history minus shared ancestors.
      expect(sym).toContain(repo.headId());
      expect(sym.length).toBeGreaterThanOrEqual(1);
    }
    expect(() => repo.revWalk([], { range: `${"x".repeat(40)}...${"y".repeat(40)}` })).toThrow(
      /couldn't parse revision|merge base/,
    );

    expect(() => repo.revWalk(["HEAD"], { sort: "topo" })).toThrow(/invalid sort/);
    expect(() => repo.revWalk([], { range: "bogus..also-bogus" })).toThrow();
  });

  test("revParseExt() resolves the spec and the reference that provided it", () => {
    const ext = repo.revParseExt("HEAD");
    expect(ext.id).toBe(repo.headId());
    expect(ext.referenceName).toBe("HEAD");

    const branch = execSync("git rev-parse --abbrev-ref HEAD", {
      cwd: cloneDir,
      encoding: "utf8",
    }).trim();
    const byBranch = repo.revParseExt(branch);
    expect(byBranch.id).toBe(repo.headId());
    expect(byBranch.referenceName).toBe(`refs/heads/${branch}`);

    // A hex id resolves, but no reference is implicated.
    const byOid = repo.revParseExt(repo.headId());
    expect(byOid.id).toBe(repo.headId());
    expect(byOid.referenceName ?? null).toBe(null);
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

  test("resolve() returns an Object directly (avoids a follow-up findObject)", () => {
    const oid = repo.headId();
    const obj = repo.resolve("HEAD");
    expect(obj.id).toBe(oid);
    expect(obj.kind).toBe("commit");
    // Equivalent to parsing the id with revParseSingle and then findObject.
    expect(repo.findObject(oid).id).toBe(obj.id);
  });

  test("revWalk() accepts a limit option", () => {
    const head = repo.revParseSingle("HEAD");
    const full = repo.revWalk([head]);
    // limit never exceeds the real history length.
    const capped = repo.revWalk([head], { limit: 50 });
    expect(capped.length).toBe(full.length);
    if (full.length >= 3) {
      expect(repo.revWalk([head], { limit: 2 }).length).toBe(2);
    }
  });
});
