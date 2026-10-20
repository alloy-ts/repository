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
  fixture = mkdtempSync(join(tmpdir(), "repository-config-pathspec-"));
  git("init --initial-branch=main", fixture);
  git("config user.email test@example.com", fixture);
  git("config user.name Test", fixture);
  // The dev machine signs commits globally; the throwaway fixture has no gpg, so disable it.
  git("config commit.gpgsign false", fixture);
  // Local config values for snapshot lookups.
  git("config custom.name gitoxide", fixture);
  git("config custom.flag true", fixture);
  git("config custom.count 7", fixture);

  writeFileSync(join(fixture, "src.ts"), "export const a = 1;\n");
  writeFileSync(join(fixture, "README.md"), "hi\n");
  git("add -A", fixture);
  git("commit -m init", fixture);

  repo = Repository.open(fixture);
});

afterAll(() => {
  rmSync(fixture, { recursive: true, force: true });
});

describe("Repository pathspec", () => {
  test("pathspec() matches included and excluded paths", () => {
    // NOTE: gix pathspec matching is first-match-wins over the ordered patterns, so a
    // negation must precede the pattern it excludes to take effect (unlike `git ls-files`,
    // which filters iteratively).
    const ps = repo.pathspec([":!src/ignore.ts", "src"]);
    expect(ps).not.toBeNull();

    // `src` matches the directory and its contents (like `git ls-files -- src`).
    expect(ps!.isIncluded("src/a.ts")).toBe(true);
    // Negative match: it matches, but is excluded from the set.
    expect(ps!.matches("src/ignore.ts")).toBe(true);
    expect(ps!.isIncluded("src/ignore.ts")).toBe(false);
    // Outside the pathspec entirely (a sibling file, not inside `src/`).
    expect(ps!.matches("src.ts")).toBe(false);
    expect(ps!.matches("README.md")).toBe(false);
  });

  test("pathspec() supports glob magic", () => {
    const ps = repo.pathspec([":(glob)src/*.ts"]);
    expect(ps!.isIncluded("src/a.ts")).toBe(true);
    expect(ps!.isIncluded("src.ts")).toBe(false);
    expect(ps!.isIncluded("README.md")).toBe(false);
  });
});
