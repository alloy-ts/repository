import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";

import {
  pathIsAbsolute,
  pathNormalize,
  pathNormalizeAndClean,
  pathNormalizeSaturating,
  pathRealpath,
  pathRealpathWithCwd,
  pathRelativizeWithPrefix,
  pathToNativeSeparators,
  pathToUnixSeparators,
} from "./main.ts";

const CWD = "/cwd";
let fixture: string;

beforeAll(() => {
  fixture = mkdtempSync(join(tmpdir(), "repository-path-"));
  mkdirSync(join(fixture, "real"), { recursive: true });
  writeFileSync(join(fixture, "real", "file.txt"), "content\n");
  // Symlink to a real dir, plus a self-referential loop for limit tests.
  symlinkSync("real", join(fixture, "link"));
  symlinkSync("loop", join(fixture, "loop"));
});

afterAll(() => {
  rmSync(fixture, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 200,
  });
});

describe("pathRealpath", () => {
  test("resolves symlinks and relative paths against the process cwd", () => {
    // Resolves through the absolute input; on macOS /tmp is a symlink itself.
    expect(pathRealpath("/tmp").endsWith("tmp")).toBe(true);
    // Non-existing components are kept as-is, not an error.
    expect(pathRealpath("/definitely/missing/path")).toBe("/definitely/missing/path");
    // The default symlink limit is 32; an explicit limit also works. The absolute
    // result may differ from the input when a prefix (e.g. macOS /var) is a symlink.
    expect(pathRealpath(fixture, 32)).toBe(pathRealpath(fixture));
  });

  test("pathRealpathWithCwd resolves relative paths against an explicit cwd", () => {
    expect(pathRealpathWithCwd("link/file.txt", fixture)).toBe(join(fixture, "real/file.txt"));
    // Missing components stay verbatim.
    expect(pathRealpathWithCwd("does/not/exist", fixture)).toBe(join(fixture, "does/not/exist"));
  });

  test("enforces the symlink limit and rejects empty paths", () => {
    // `loop` points at itself; a limit of 1 must trip.
    expect(() => pathRealpathWithCwd("loop", fixture, 1)).toThrow(/symlinks/);
    expect(() => pathRealpath("")).toThrow();
  });

  test("rejects .. escaping the filesystem root", () => {
    expect(() => pathRealpathWithCwd("/..", fixture)).toThrow();
  });
});

describe("pathNormalize", () => {
  // Cases from the upstream doctest.
  test("eliminates intermediate .. virtually", () => {
    expect(pathNormalize("a/./b/c/.././..", CWD)).toBe("a");
    expect(pathNormalize("/a/../b/..", CWD)).toBe("/");
    expect(pathNormalize("/base/a/..", CWD)).toBe("/base");
    expect(pathNormalize("./a/..", CWD)).toBe(".");
    expect(pathNormalize("/base/../base", CWD)).toBe("/base");
  });

  test("preserves leading dots and duplicate separators", () => {
    expect(pathNormalize(".///", CWD)).toBe(".///");
    expect(pathNormalize("a//b", CWD)).toBe("a//b");
  });

  test("returns null when .. exhausts currentDir", () => {
    expect(pathNormalize("../../r", "/c")).toBeNull();
    expect(pathNormalize("./a/../..", CWD)).toBe("/");
  });
});

describe("pathNormalizeSaturating", () => {
  test("clamps .. at the filesystem root instead of returning null", () => {
    expect(pathNormalizeSaturating("../../r", "/c")).toBe("/r");
    expect(pathNormalizeSaturating("/base/a/..", CWD)).toBe("/base");
  });
});

describe("pathNormalizeAndClean", () => {
  test("additionally removes dot components and duplicate separators", () => {
    expect(pathNormalizeAndClean("a//b/.", CWD)).toBe("a/b");
    expect(pathNormalizeAndClean("./a/../..", CWD)).toBe("/");
    expect(pathNormalizeAndClean("/base/../base", CWD)).toBe("/base");
  });

  test("returns currentDir when cleaning leaves no components", () => {
    // No `..` to normalize, but cleaning strips the dot components entirely.
    expect(pathNormalizeAndClean(".///", "/base")).toBe("/base");
    // `a/../..` normalizes to "/" (the popped current dir), not to currentDir.
    expect(pathNormalizeAndClean("a/../..", "/base")).toBe("/");
  });
});

describe("pathRelativizeWithPrefix", () => {
  test("rebuilds paths relative to the prefix", () => {
    expect(pathRelativizeWithPrefix("src/lib/main.rs", "src/lib")).toBe("main.rs");
    // Crossing out of the prefix produces ../ components.
    expect(pathRelativizeWithPrefix("src/lib/main.rs", "docs")).toBe("../src/lib/main.rs");
    expect(pathRelativizeWithPrefix("src/main.rs", "src/lib")).toBe("../main.rs");
  });

  test("is a no-op for an empty prefix and yields . for identical paths", () => {
    expect(pathRelativizeWithPrefix("src/lib/main.rs", "")).toBe("src/lib/main.rs");
    expect(pathRelativizeWithPrefix("src", "src")).toBe(".");
  });
});

describe("pathIsAbsolute", () => {
  test("accepts platform-absolute and Unix-style absolute paths", () => {
    expect(pathIsAbsolute("/tmp")).toBe(true);
    expect(pathIsAbsolute("a/b")).toBe(false);
    expect(pathIsAbsolute("")).toBe(false);
    // A lone leading slash counts as absolute even on non-Unix platforms.
    expect(pathIsAbsolute("/relative-looking")).toBe(true);
  });
});

describe("separator conversion", () => {
  test("toUnixSeparators always converts backslashes to slashes", () => {
    expect(pathToUnixSeparators("a\\b\\c")).toBe("a/b/c");
    expect(pathToUnixSeparators("a/b/c")).toBe("a/b/c");
  });

  test("toNativeSeparators is a no-op on Unix", () => {
    expect(pathToNativeSeparators("a/b/c")).toBe("a/b/c");
  });
});
