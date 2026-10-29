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
  base = mkdtempSync(join(tmpdir(), "repo-rev-walk-"));
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

describe("Repository rev-walk (src/rev_walk.rs)", () => {
  test("revWalk() returns the tip first and at least one commit", () => {
    const tip = repo.revParseSingle("HEAD");
    const walked = repo.revWalk([tip]);
    expect(walked[0]).toBe(tip);
    expect(walked.length).toBeGreaterThanOrEqual(1);
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

describe("Repository Revwalk (src/rev_walk.rs)", () => {
  test("push/next walks from a tip and matches revWalk", () => {
    const tip = repo.revParseSingle("HEAD");
    const walk = repo.revwalk();
    walk.push(tip);
    const ids: string[] = [];
    let id: string | null;
    while ((id = walk.next()) !== null) {
      ids.push(id);
    }
    expect(ids[0]).toBe(tip);
    expect(ids).toEqual(repo.revWalk([tip]));
  });

  test("pushHead / pushRef / hide / hideHead / hideRef", () => {
    const head = repo.revParseSingle("HEAD");

    const w1 = repo.revwalk();
    w1.pushHead();
    expect(w1.toArray()[0]).toBe(head);

    // hide the tip empties the walk.
    const w2 = repo.revwalk();
    w2.pushHead();
    w2.hide(head);
    expect(w2.toArray()).toEqual([]);

    // hideHead hides HEAD.
    const w3 = repo.revwalk();
    w3.pushHead();
    w3.hideHead();
    expect(w3.toArray()).toEqual([]);

    // pushRef("HEAD") / hideRef("HEAD") resolve the ref's commit.
    const w4 = repo.revwalk();
    w4.pushRef("HEAD");
    expect(w4.toArray()[0]).toBe(head);

    const w5 = repo.revwalk();
    w5.pushHead();
    w5.hideRef("HEAD");
    expect(w5.toArray()).toEqual([]);
  });

  test("pushGlob / hideGlob match references", () => {
    execSync("git tag probe-glob-tag HEAD", { cwd: cloneDir, stdio: "ignore" });
    try {
      const ids = (() => {
        const w = repo.revwalk();
        w.pushGlob("refs/tags/*");
        return w.toArray();
      })();
      expect(ids.length).toBeGreaterThan(0);

      const w2 = repo.revwalk();
      w2.pushGlob("tags/*");
      expect(w2.toArray()).toEqual(ids);

      const w3 = repo.revwalk();
      w3.pushGlob("refs/tags");
      expect(w3.toArray()).toEqual(ids);

      // hideGlob removes those refs' ancestors.
      const w4 = repo.revwalk();
      w4.pushHead();
      w4.hideGlob("refs/tags/*");
      expect(w4.toArray()).not.toContain(repo.revParseSingle("HEAD"));
    } finally {
      execSync("git tag -d probe-glob-tag", { cwd: cloneDir, stdio: "ignore" });
    }
  });

  test("setSorting accepts Time and Reverse", () => {
    const head = repo.revParseSingle("HEAD");

    const w1 = repo.revwalk();
    w1.push(head);
    const def = w1.toArray();
    expect(def[0]).toBe(head);

    const w2 = repo.revwalk();
    w2.push(head);
    w2.setSorting(["Time"]);
    expect(w2.toArray().length).toBe(def.length);

    const w3 = repo.revwalk();
    w3.push(head);
    w3.setSorting(["Time", "Reverse"]);
    const rev = w3.toArray();
    expect(rev.length).toBe(def.length);
    expect(rev).toEqual([...def].reverse());
  });

  test("simplifyFirstParent reduces or equals the full walk", () => {
    const head = repo.revParseSingle("HEAD");
    const w = repo.revwalk();
    w.push(head);
    w.simplifyFirstParent();
    const fp = w.toArray();
    expect(fp.length).toBeGreaterThanOrEqual(1);
    expect(fp.length).toBeLessThanOrEqual(repo.revWalk([head]).length);
  });

  test("pushRange hides the left side and pushes the right", () => {
    const parent = repo.headCommit().parentIds[0];
    if (parent) {
      const w1 = repo.revwalk();
      w1.pushRange(`${parent}..HEAD`);
      expect(w1.toArray()).toEqual(repo.revWalk([], { range: `${parent}..HEAD` }));
      expect(w1.toArray()).not.toContain(parent);

      const w2 = repo.revwalk();
      w2.pushRange(`${parent}...HEAD`);
      expect(w2.toArray()).toEqual(repo.revWalk([], { range: `${parent}...HEAD` }));
    }
  });

  test("next() returns null after exhausting; reset re-runs the walk", () => {
    const head = repo.revParseSingle("HEAD");
    const walk = repo.revwalk();
    walk.push(head);
    const all = walk.toArray();

    const drained: string[] = [];
    let id: string | null;
    while ((id = walk.next()) !== null) drained.push(id);
    expect(drained).toEqual(all);
    expect(walk.next()).toBe(null);

    // reset re-runs the same configured walk.
    walk.reset();
    expect(walk.next()).toBe(all[0]);
  });

  test("withHideCallback hides commits the callback marks", () => {
    const head = repo.revParseSingle("HEAD");

    const w1 = repo.revwalk();
    w1.pushHead();
    w1.withHideCallback((oid: string) => oid === head);
    expect(w1.toArray()).toEqual([]);

    const w2 = repo.revwalk();
    w2.pushHead();
    w2.withHideCallback(() => false);
    expect(w2.toArray()).toEqual(repo.revWalk(["HEAD"]));
  });
});
