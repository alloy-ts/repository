import { execSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { refNameNormalise, Repository } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-reference-"));
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
  rmSync(base, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 200,
  });
});

describe("Repository.createSymbolicReference (src/reference.rs)", () => {
  const name = "refs/heads/reference-test-sym";
  const target = "refs/heads/main";

  test("creates a symbolic ref pointing at another ref", () => {
    const ref = repo.createSymbolicReference(name, target, false);
    expect(ref.name).toBe(name);
    expect(ref.isSymbolic()).toBe(true);
    expect(ref.symbolicTarget).toBe(target);
    expect(ref.target).toBe(""); // symbolic refs carry no direct object id
  });

  test("rejects overwrite without force, then overwrites with force", () => {
    const n = "refs/heads/reference-test-overwrite";
    const ref = repo.createSymbolicReference(n, "refs/heads/main", false);
    expect(ref.symbolicTarget).toBe("refs/heads/main");
    // A second create without force must fail because the ref now exists.
    expect(() => repo.createSymbolicReference(n, "refs/heads/main", false)).toThrow();
    const ref2 = repo.createSymbolicReference(n, "refs/heads/main", true, "overwrite");
    expect(ref2.symbolicTarget).toBe("refs/heads/main");
    repo.findReference(n)?.delete();
  });

  test("DWIM-resolves a short target name", () => {
    const short = repo.createSymbolicReference("refs/heads/reference-test-sym2", "main", false);
    expect(short.symbolicTarget).toBe("refs/heads/main");
  });

  test("cleanup: remove the test refs", () => {
    repo.findReference(name)?.delete();
    repo.findReference("refs/heads/reference-test-sym2")?.delete();
  });
});

describe("refNameNormalise (src/reference.rs)", () => {
  test("collapses adjacent slashes (NORMAL)", () => {
    expect(refNameNormalise("foo//bar")).toBe("foo/bar");
  });

  test("allows one-level names with allowOneLevel", () => {
    expect(refNameNormalise("HEAD", { allowOneLevel: true })).toBe("HEAD");
  });

  test("allows shorthand branches with allowOneLevel + refspecShorthand", () => {
    expect(refNameNormalise("main", { allowOneLevel: true, refspecShorthand: true })).toBe("main");
  });

  test("allows a single '*' as a component with refspecPattern", () => {
    expect(refNameNormalise("refs/heads/*", { refspecPattern: true })).toBe("refs/heads/*");
    expect(refNameNormalise("foo/*/bar", { refspecPattern: true })).toBe("foo/*/bar");
  });

  test("rejects names that are invalid for the chosen mode", () => {
    // "main" is not a valid full name (NORMAL).
    expect(() => refNameNormalise("main")).toThrow();
    // Two '*' are never allowed.
    expect(() => refNameNormalise("foo/*/bar*", { refspecPattern: true })).toThrow();
  });
});

describe("Reference.isSymbolic (src/reference.rs)", () => {
  test("direct refs are not symbolic; symbolic refs are", () => {
    const main = repo.findReference("refs/heads/main");
    expect(main).not.toBe(null);
    expect(main!.isSymbolic()).toBe(false);

    const sym = repo.createSymbolicReference(
      "refs/heads/reference-test-issym",
      "refs/heads/main",
      true,
    );
    expect(sym.isSymbolic()).toBe(true);
    repo.findReference("refs/heads/reference-test-issym")?.delete();
  });
});
