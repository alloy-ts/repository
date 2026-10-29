import { execSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";

import { Repository } from "./main.ts";

// Same style as the passing config_v1 test: let the shell resolve `git`.
function git(args: string, cwd: string) {
  // Allow local file:// submodule URLs (modern git blocks them by default).
  execSync(`git -c protocol.file.allow=always ${args}`, {
    cwd,
    stdio: "ignore",
  });
}

// ---------------------------------------------------------------------------
// Fixture 1: a parent repository with a fully-checked-out submodule.
// Used by the read (Tier A) and write (Phase 1) suites, which require an
// already-initialised submodule (a real HEAD to read / record).
// ---------------------------------------------------------------------------
let fixture: string;
let repo: Repository;

beforeAll(() => {
  fixture = mkdtempSync(join(tmpdir(), "repository-submod-"));

  // A standalone "remote" child repository.
  const child = join(fixture, "child-src");
  mkdirSync(child, { recursive: true });
  git("init --initial-branch=main", child);
  git("config user.email c@e.com", child);
  git("config user.name C", child);
  git("config commit.gpgsign false", child);
  writeFileSync(join(child, "a.txt"), "hello\n");
  git("add a.txt", child);
  git("commit -m init", child);

  // A parent repository with the child added as a submodule and checked out.
  const parent = join(fixture, "parent");
  mkdirSync(parent, { recursive: true });
  git("init --initial-branch=main", parent);
  git("config user.email p@e.com", parent);
  git("config user.name P", parent);
  git("config commit.gpgsign false", parent);
  git(`submodule add file://${child} child`, parent);
  git("commit -m add-submodule", parent);
  git("submodule update --init", parent);

  repo = Repository.open(parent);
});

afterAll(() => {
  rmSync(fixture, { recursive: true, force: true });
});

describe("Submodule read API (Tier A)", () => {
  test("submodules() lists the configured submodule, name/path/url resolve", () => {
    const subs = repo.submodules();
    expect(subs.length).toBe(1);
    const sm = subs[0];
    expect(sm.name()).toBe("child");
    expect(sm.validatedName()).toBe("child");
    expect(sm.path()).toBe("child");
    expect(sm.url()).not.toBeNull();
    expect(sm.url()!.startsWith("file://")).toBe(true);
  });

  test("override-aware config getters default to null when unset", () => {
    const sm = repo.submodule("child")!;
    expect(sm.branch()).toBeNull();
    expect(sm.updateStrategy()).toBeNull();
    expect(sm.ignoreRule()).toBeNull();
    expect(sm.fetchRecurse()).toBeNull();
    expect(sm.shallow()).toBeNull();
  });

  test("status() agrees with headId/indexId/workdirId on a clean checkout", () => {
    const sm = repo.submodule("child")!;
    const st = sm.status();
    expect(st.indexId).not.toBeNull();
    expect(st.checkedOutHeadId).not.toBeNull();
    expect(st.indexId).toBe(st.checkedOutHeadId);
    expect(st.isDirty).toBe(false);
    expect(st.state.repositoryExists).toBe(true);
    expect(st.state.worktreeCheckout).toBe(true);
    // The handle getters should corroborate the status summary.
    expect(sm.headId()).toBe(st.indexId);
    expect(sm.indexId()).toBe(st.indexId);
    expect(sm.workdirId()).toBe(st.checkedOutHeadId);
  });

  test("status() detects a dirty submodule worktree", () => {
    writeFileSync(join(fixture, "parent", "child", "dirty.txt"), "x\n");
    const sm = repo.submodule("child")!;
    const st = sm.status({ checkDirty: true });
    // Only the worktree is dirty; the recorded commit is unchanged.
    expect(st.isDirty).toBe(true);
    expect(st.checkedOutHeadId).toBe(st.indexId);
  });

  test("submoduleStatuses() returns one agreement entry", () => {
    const all = repo.submoduleStatuses();
    expect(all.length).toBe(1);
    expect(all[0].indexId).not.toBeNull();
  });

  test("modules() exposes the parsed .gitmodules", () => {
    const mf = repo.modules()!;
    expect(mf.names().sort()).toEqual(["child"]);
    expect(mf.path("child")).toBe("child");
    expect(mf.url("child")).toBe(`file://${join(fixture, "child-src")}`);
  });
});

describe("Submodule write API (Phase 1)", () => {
  test("init() copies .gitmodules config into local .git/config", () => {
    const sm = repo.submodule("child")!;
    sm.init();
    // After init, the local config should carry the submodule url.
    const cfg = readFileSync(join(fixture, "parent", ".git", "config"), "utf8");
    expect(cfg).toMatch(/submodule "child"/);
    expect(cfg).toMatch(/url = file:\/\//);
  });

  test("sync() updates the submodule's remote.origin.url", () => {
    const sm = repo.submodule("child")!;
    // Rewrite the submodule's remote to something bogus, then sync it back.
    const subCfg = join(fixture, "parent", "child", ".git", "config");
    const before = readFileSync(subCfg, "utf8");
    writeFileSync(subCfg, before.replace(/url = file:[^\n]*/, "url = file:///bogus"));
    sm.sync();
    const after = readFileSync(subCfg, "utf8");
    expect(after).toMatch(/url = file:\/\//);
    expect(after).not.toMatch(/url = file:\/\/\/bogus/);
  });

  test("addToIndex() records the checked-out commit as the gitlink", () => {
    const sm = repo.submodule("child")!;
    sm.addToIndex();
    expect(sm.indexId()).toBe(sm.headId());
  });

  test("addFinalise() stages .gitmodules into the parent index", () => {
    const sm = repo.submodule("child")!;
    // Touch .gitmodules so its blob differs, then finalise.
    writeFileSync(
      join(fixture, "parent", ".gitmodules"),
      '[submodule "child"]\n\tpath = child\n\turl = file://x\n',
    );
    sm.addFinalise();
    // The staged blob should be re-readable from the index as a gitlink companion.
    const staged = execSync("git ls-files --stage .gitmodules", {
      cwd: join(fixture, "parent"),
      encoding: "utf8",
    });
    expect(staged).toMatch(/100644 .*?\t\.gitmodules/);
  });
});

// ---------------------------------------------------------------------------
// Fixture 2: a bare parent (no submodule yet) plus a child-src. Drives the
// add-from-scratch lifecycle (addSubmodule -> repoInit -> clone -> update).
// ---------------------------------------------------------------------------
let p2: string;
let childSrc: string;
let parent2: string;

describe("Submodule lifecycle API (Phase 2)", () => {
  beforeAll(() => {
    p2 = mkdtempSync(join(tmpdir(), "repository-submod-p2-"));
    childSrc = join(p2, "child-src");
    parent2 = join(p2, "parent");

    mkdirSync(childSrc, { recursive: true });
    git("init --initial-branch=main", childSrc);
    git("config user.email c@e.com", childSrc);
    git("config user.name C", childSrc);
    git("config commit.gpgsign false", childSrc);
    writeFileSync(join(childSrc, "a.txt"), "hello\n");
    git("add a.txt", childSrc);
    git("commit -m init", childSrc);

    mkdirSync(parent2, { recursive: true });
    git("init --initial-branch=main", parent2);
    git("config user.email p@e.com", parent2);
    git("config user.name P", parent2);
    git("config commit.gpgsign false", parent2);

    repo = Repository.open(parent2);
  });

  afterAll(() => {
    rmSync(p2, { recursive: true, force: true });
  });

  test("addSubmodule registers the submodule in .gitmodules", () => {
    // Register two submodules at distinct paths so repoInit and clone don't collide.
    repo.addSubmodule(`file://${childSrc}`, "child");
    repo.addSubmodule(`file://${childSrc}`, "childInit");
    const subs = repo.submodules();
    expect(subs.length).toBe(2);
    expect(subs.map((s) => s.name()).sort()).toEqual(["child", "childInit"]);
    const mf = repo.modules()!;
    expect(mf.path("child")).toBe("child");
    expect(mf.url("child")).toBe(`file://${childSrc}`);
  });

  test("repoInit(useGitlink=true) creates the gitdir-file layout", () => {
    const sm = repo.submodule("childInit")!;
    sm.repoInit(true);
    const gitlink = join(parent2, "childInit", ".git");
    expect(existsSync(gitlink)).toBe(true);
    expect(readFileSync(gitlink, "utf8")).toMatch(/gitdir:/);
    expect(existsSync(join(parent2, ".git", "modules", "childInit"))).toBe(true);
  });

  test("clone() clones, checks out, and records the gitlink", () => {
    const sm = repo.submodule("child")!;
    sm.clone();
    // Worktree content is checked out.
    expect(existsSync(join(parent2, "child", "a.txt"))).toBe(true);
    // Standard submodule gitdir-file layout.
    const gitlink = join(parent2, "child", ".git");
    expect(readFileSync(gitlink, "utf8")).toMatch(/gitdir:.*\.git\/modules\/child/);
    // Openable and HEAD present.
    expect(sm.open()).not.toBeNull();
    const head = sm.headId();
    expect(head).not.toBeNull();
    // Gitlink recorded in the parent index equals the checked-out commit.
    expect(sm.indexId()).toBe(head);
  });

  test("update() succeeds on an already-cloned submodule", () => {
    const sm = repo.submodule("child")!;
    sm.update();
    expect(existsSync(join(parent2, "child", "a.txt"))).toBe(true);
  });
});
