import { execSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";

import { isPathspecValid, parsePathspec, Repository } from "./main.ts";

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
  mkdirSync(join(fixture, "src"), { recursive: true });
  mkdirSync(join(fixture, "docs"), { recursive: true });
  writeFileSync(join(fixture, "src", "a.ts"), "export const b = 2;\n");
  writeFileSync(join(fixture, "src", "ignore.ts"), "export const c = 3;\n");
  writeFileSync(join(fixture, "docs", "readme.md"), "# docs\n");
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

  test("patterns() exposes the parsed patterns with their magic", () => {
    const ps = repo.pathspec([":!src/ignore.ts", "src"]);
    const patterns = ps!.patterns();
    expect(patterns.length).toBe(2);

    expect(patterns[0]!.isExcluded).toBe(true);
    expect(patterns[0]!.path).toBe("src/ignore.ts");
    expect(patterns[0]!.magic).toContain("exclude");

    expect(patterns[1]!.isExcluded).toBe(false);
    expect(patterns[1]!.path).toBe("src");
    expect(patterns[1]!.magic).toEqual([]);

    const globbed = repo.pathspec([":(glob,top)src/*.ts"]).patterns()[0]!;
    expect(globbed.searchMode).toBe("PathAwareGlob");
    expect(globbed.magic).toEqual(["top"]);
  });

  test("commonPrefix() / longestCommonDirectory() describe the search scope", () => {
    const ps = repo.pathspec([":!src/ignore.ts", "src"]);
    // Only positive patterns participate.
    expect(ps!.commonPrefix()).toBe("src");
    expect(ps!.longestCommonDirectory()).toBe("src");

    const everything = repo.pathspec([":"]);
    expect(everything.patterns()[0]!.isNil).toBe(true);
    expect(everything.commonPrefix()).toBe("");
  });

  test("match() reports how a path relates to the pathspecs", () => {
    const ps = repo.pathspec([":!src/ignore.ts", "src"]);

    const included = ps!.match("src/a.ts");
    expect(included).not.toBeNull();
    expect(included!.included).toBe(true);
    expect(included!.excluded).toBe(false);
    expect(included!.kind).toBe("Prefix");
    expect(included!.patternIndex).toBe(1);

    const excluded = ps!.match("src/ignore.ts");
    expect(excluded!.included).toBe(false);
    expect(excluded!.excluded).toBe(true);
    expect(excluded!.patternIndex).toBe(0);

    expect(ps!.match("src.ts")).toBeNull();
  });

  test("matchIndex() lists the index entries selected by the pathspec", () => {
    const ps = repo.pathspec([":!src/ignore.ts", "src"]);
    const entries = ps!.matchIndex();
    expect(entries.map((e) => e.path)).toEqual(["src/a.ts"]);

    const none = repo.pathspec(["no-such-dir"]).matchIndex();
    expect(none).toEqual([]);
  });

  test("mirrors the defaults with case inheritance on", () => {
    const base = repo.pathspecDefaults();
    const inherited = repo.pathspecDefaultsInheritIgnoreCase(true);
    expect(inherited.searchMode).toBe(base.searchMode);
    expect(inherited.literal).toBe(base.literal);
    expect(base.searchMode).toBe("ShellGlob");
    expect(base.literal).toBe(false);
    expect(Array.isArray(base.magic)).toBe(true);
    expect(typeof base.ignoreCase).toBe("boolean");
  });
});

describe("parsePathspec() / isPathspecValid()", () => {
  test("parsePathspec() resolves magic without a repository", () => {
    const p = parsePathspec(":(glob,icase)docs/*.md");
    expect(p.path).toBe("docs/*.md");
    expect(p.searchMode).toBe("PathAwareGlob");
    expect(p.magic).toEqual(["icase"]);
    expect(p.isExcluded).toBe(false);
    expect(p.isNil).toBe(false);

    const excluded = parsePathspec(":!build");
    expect(excluded.isExcluded).toBe(true);
    expect(excluded.magic).toEqual(["exclude"]);
    expect(excluded.path).toBe("build");

    const nil = parsePathspec(":");
    expect(nil.isNil).toBe(true);
  });

  test("parsePathspec() honors the given defaults", () => {
    const p = parsePathspec("build", { ignoreCase: true, literal: true });
    expect(p.path).toBe("build");
    expect(p.magic).toEqual(["icase"]);
    expect(p.searchMode).toBe("Literal");

    expect(parsePathspec("src/*.ts", { searchMode: "PathAwareGlob" }).searchMode).toBe(
      "PathAwareGlob",
    );
    expect(() => parsePathspec("src", { searchMode: "nope" })).toThrow(/search mode/);
  });

  test("isPathspecValid() rejects malformed magic", () => {
    expect(isPathspecValid("src/**/*.ts")).toBe(true);
    expect(isPathspecValid(":(exclude,top)build")).toBe(true);
    expect(isPathspecValid(":(nope)x")).toBe(false);
  });
});
