import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";

import { AttributeSource, Repository } from "./main.ts";

let dir: string;
let repo: Repository;

beforeAll(() => {
  dir = mkdtempSync(join(tmpdir(), "repo-attrs-"));
  // Initialize a throwaway repo with known attribute/ignore rules so the tests
  // are deterministic regardless of the host project's configuration.
  execSync("git init -q -b main", { cwd: dir, stdio: "ignore" });
  writeFileSync(join(dir, ".gitattributes"), "*.txt myattr=hello\nsecret.bin binary\n");
  writeFileSync(join(dir, ".gitignore"), "*.log\n");
  writeFileSync(join(dir, "foo.txt"), "hello\n");
  writeFileSync(join(dir, "secret.bin"), Buffer.from([0, 1, 2, 3]));
  writeFileSync(join(dir, "debug.log"), "log\n");
  execSync("git -c user.email=t@t.t -c user.name=t add -A", {
    cwd: dir,
    stdio: "ignore",
  });
  execSync("git -c user.email=t@t.t -c user.name=t -c commit.gpgsign=false commit -q -m init", {
    cwd: dir,
    stdio: "ignore",
  });
  repo = Repository.open(dir);
});

afterAll(() => {
  rmSync(dir, { recursive: true, force: true });
});

describe("Repository.attributes (src/attributes.rs)", () => {
  test("resolves .gitattributes rules into typed entries", () => {
    const entries = repo.attributes("foo.txt");
    expect(Array.isArray(entries)).toBe(true);
    for (const e of entries) {
      expect(typeof e.name).toBe("string");
      expect(["set", "unset", "value", "unspecified"]).toContain(e.state);
      if (e.state !== "value") expect(e.value ?? null).toBe(null);
    }
    // `myattr=hello` resolves as the `value` state carrying "hello".
    const myattr = entries.find((e) => e.name === "myattr");
    expect(myattr).toBeDefined();
    expect(myattr!.state).toBe("value");
    expect(myattr!.value).toBe("hello");
  });

  test("marks a binary file via the built-in `binary` attribute", () => {
    const entries = repo.attributes("secret.bin");
    const binary = entries.find((e) => e.name === "binary");
    expect(binary).toBeDefined();
    expect(binary!.state).toBe("set");
    expect(binary!.value ?? null).toBe(null);
  });

  test("accepts an explicit AttributeSource and still returns entries", () => {
    const worktree = repo.attributes("foo.txt", AttributeSource.WorktreeThenIdMapping);
    const idMapping = repo.attributes("foo.txt", AttributeSource.IdMapping);
    expect(Array.isArray(worktree)).toBe(true);
    expect(Array.isArray(idMapping)).toBe(true);
    expect(worktree.find((e) => e.name === "myattr")).toBeDefined();
  });
});

describe("Repository.getAttribute (git2's get_attr)", () => {
  test("resolves a single attribute, or null when unspecified", () => {
    const valued = repo.getAttribute("foo.txt", "myattr");
    expect(valued).not.toBeNull();
    expect(valued!.state).toBe("value");
    expect(valued!.value).toBe("hello");

    const set = repo.getAttribute("secret.bin", "binary");
    expect(set!.state).toBe("set");
    expect(set!.value ?? null).toBe(null);

    expect(repo.getAttribute("foo.txt", "no-such-attr")).toBeNull();
  });

  test("is available on the reusable stack, too", () => {
    const stack = repo.attributesStack();
    expect(stack.getAttribute("foo.txt", "myattr")!.value).toBe("hello");
    expect(stack.getAttribute("foo.txt", "no-such-attr")).toBeNull();
    // The stack still answers list and exclude queries.
    expect(stack.attributes("foo.txt").find((e) => e.name === "myattr")).toBeDefined();
    expect(stack.isExcluded("debug.log")).toBe(true);
  });
});

describe("Repository.attributesOnly", () => {
  test("resolves .gitattributes without consulting ignore sources", () => {
    const entries = repo.attributesOnly("foo.txt");
    expect(Array.isArray(entries)).toBe(true);
    expect(entries.find((e) => e.name === "myattr")).toBeDefined();
  });
});

describe("Repository.attributesWithStatistics", () => {
  test("returns entries paired with numeric statistics", () => {
    const res = repo.attributesWithStatistics("foo.txt");
    expect(Array.isArray(res.entries)).toBe(true);
    expect(res.entries.find((e) => e.name === "myattr")?.value).toBe("hello");

    expect(typeof res.statistics.patternsBuffers).toBe("number");
    expect(typeof res.statistics.patternFiles).toBe("number");
    expect(typeof res.statistics.triedPatternFiles).toBe("number");
    expect(res.statistics.patternsBuffers).toBeGreaterThanOrEqual(0);
    expect(res.statistics.patternFiles).toBeGreaterThanOrEqual(0);
  });
});

describe("Repository.attributesOnlyWithStatistics", () => {
  test("returns entries paired with numeric statistics", () => {
    const res = repo.attributesOnlyWithStatistics("foo.txt");
    expect(Array.isArray(res.entries)).toBe(true);
    expect(res.entries.find((e) => e.name === "myattr")).toBeDefined();
    expect(typeof res.statistics.patternFiles).toBe("number");
    expect(res.statistics.patternFiles).toBeGreaterThanOrEqual(0);
  });
});

describe("Repository.isPathExcluded", () => {
  test("honors .gitignore patterns", () => {
    expect(repo.isPathExcluded("debug.log")).toBe(true);
    expect(repo.isPathExcluded("foo.txt")).toBe(false);
  });
});
