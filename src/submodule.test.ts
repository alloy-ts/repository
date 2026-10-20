import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";

import { Repository } from "./main.ts";

let fixture: string;
let repo: Repository;

function git(args: string, cwd: string) {
  execSync(`git ${args}`, { cwd, stdio: "ignore" });
}

beforeAll(() => {
  // A throwaway repository that actually contains a `.gitmodules` file.
  // Committed so `modules()` (which reads the HEAD tree) and `open_modules_file()`
  // (which reads the worktree) can both resolve it.
  fixture = mkdtempSync(join(tmpdir(), "repository-submodule-"));
  git("init --initial-branch=main", fixture);
  git("config user.email test@example.com", fixture);
  git("config user.name Test", fixture);
  // The dev machine signs commits globally; the throwaway fixture has no gpg, so disable it.
  git("config commit.gpgsign false", fixture);

  const gitmodules = [
    '[submodule "libfoo"]',
    "\tpath = lib/foo",
    "\turl = https://example.com/foo.git",
    '[submodule "libbar"]',
    "\tpath = lib/bar",
    "\turl = https://example.com/bar.git",
    "\tbranch = stable",
    "\tupdate = rebase",
    "\tignore = untracked",
    "\tfetchRecurseSubmodules = on-demand",
    "\tshallow = true",
    "",
  ].join("\n");
  writeFileSync(join(fixture, ".gitmodules"), gitmodules);
  git("add .gitmodules", fixture);
  git("commit -m add-submodules", fixture);
  // Record submodule config in .git/config without cloning (offline).
  git("submodule init", fixture);

  repo = Repository.open(fixture);
});

afterAll(() => {
  rmSync(fixture, { recursive: true, force: true });
});

describe("Repository submodules (src/submodule.rs)", () => {
  test("openModulesFile() parses the worktree .gitmodules", () => {
    const mf = repo.openModulesFile();
    expect(mf).not.toBeNull();

    const names = mf!.names().sort();
    expect(names).toEqual(["libbar", "libfoo"]);

    expect(mf!.path("libfoo")).toBe("lib/foo");
    expect(mf!.url("libfoo")).toBe("https://example.com/foo.git");
  });

  test("modules() returns the same parsed configuration from HEAD", () => {
    const mf = repo.modules();
    expect(mf).not.toBeNull();

    expect(mf!.path("libbar")).toBe("lib/bar");
    expect(mf!.url("libbar")).toBe("https://example.com/bar.git");
    expect(mf!.branch("libbar")).toBe("stable");
    expect(mf!.update("libbar")).toBe("rebase");
    expect(mf!.ignore("libbar")).toBe("untracked");
    expect(mf!.fetchRecurse("libbar")).toBe("on-demand");
    expect(mf!.shallow("libbar")).toBe(true);
  });

  test("accessors error on an unknown submodule name", () => {
    const mf = repo.openModulesFile()!;
    expect(() => mf.path("does-not-exist")).toThrow();
  });
});

describe("Repository submodule handles (src/submodule.rs)", () => {
  test("submodules() lists configured handles and submodule(name) resolves one", () => {
    const subs = repo.submodules();
    const names = subs.map((s) => s.name()).sort();
    expect(names).toEqual(["libbar", "libfoo"]);

    const foo = repo.submodule("libfoo");
    expect(foo).not.toBeNull();
    expect(foo!.name()).toBe("libfoo");
    expect(foo!.gitDir()).toContain("modules/libfoo");
    expect(typeof foo!.workDir()).toBe("string");
  });

  test("submodule(name) returns null for an unknown name", () => {
    expect(repo.submodule("nope")).toBe(null);
  });

  test("state()/headId()/indexId()/isActive() reflect an uninitialized submodule", () => {
    const bar = repo.submodule("libbar")!;
    const state = bar.state();
    // configured in .gitmodules but never cloned
    expect(typeof state.superprojectConfiguration).toBe("boolean");
    expect(state.repositoryExists).toBe(false);
    expect(state.worktreeCheckout).toBe(false);
    // no index entry and no committed tree entry for the submodule path
    expect(bar.headId()).toBe(null);
    expect(bar.indexId()).toBe(null);
    expect(typeof bar.isActive()).toBe("boolean");
    // not initialized, so open() yields null
    expect(bar.open()).toBe(null);
  });
});
