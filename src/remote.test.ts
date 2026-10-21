import { execSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository } from "./main.ts";

const RemoteDirection = {
  Fetch: 0,
  Push: 1,
} as const;

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-remote-"));
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

describe("Repository remotes (src/remote.rs)", () => {
  test("remotes() returns an array; findRemote('origin') resolves", () => {
    const rs = repo.remotes();
    expect(Array.isArray(rs)).toBe(true);
    const origin = rs.find((r) => r.name === "origin");
    if (origin) {
      const o = repo.findRemote("origin");
      expect(o.name).toBe("origin");
      expect(typeof o.url).toBe("string");
    }
  });

  test("findRemote throws on a missing remote", () => {
    expect(() => repo.findRemote("__nope__")).toThrow();
  });

  test("remoteAt() returns an anonymous remote for a URL", () => {
    const r = repo.remoteAt("https://example.com/foo.git");
    expect(r.name).toBe(null);
    expect(r.url).toBe("https://example.com/foo.git");
  });

  test("createRemote() adds a named remote", () => {
    const r = repo.createRemote("wave1test", "https://example.com/bar.git");
    expect(r.name).toBe("wave1test");
    // Persisted to disk, so a fresh open observes the new remote.
    const fresh = Repository.open(cloneDir);
    expect(fresh.remotes().some((x) => x.name === "wave1test")).toBe(true);
  });

  test("remoteAtWithoutUrlRewrite() returns an anonymous remote", () => {
    const r = repo.remoteAtWithoutUrlRewrite("https://example.com/no-rewrite.git");
    expect(r.name).toBe(null);
    expect(r.url).toBe("https://example.com/no-rewrite.git");
  });

  test("tryFindRemote() resolves existing and null for missing", () => {
    const existing = repo.tryFindRemote("origin");
    expect(existing).not.toBe(null);
    expect(existing?.name).toBe("origin");
    expect(repo.tryFindRemote("__nope__")).toBe(null);
  });

  test("tryFindRemoteWithoutUrlRewrite() resolves existing remotes", () => {
    const r = repo.tryFindRemoteWithoutUrlRewrite("origin");
    expect(r).not.toBe(null);
    expect(r?.name).toBe("origin");
  });

  test("findDefaultRemote() returns the configured fetch remote", () => {
    const r = repo.findDefaultRemote(RemoteDirection.Fetch as any);
    expect(r).not.toBe(null);
    expect(typeof r?.name).toBe("string");
  });

  test("findFetchRemote() resolves a named remote", () => {
    const r = repo.findFetchRemote("origin");
    expect(r.name).toBe("origin");
    expect(typeof r.url).toBe("string");
  });

  test("remoteNames() lists the configured remotes", () => {
    const names = repo.remoteNames();
    expect(Array.isArray(names)).toBe(true);
    expect(names).toContain("origin");
    for (const n of names) expect(typeof n).toBe("string");
  });

  test("remoteDefaultName() resolves the configured branch remote", () => {
    // branch.main.remote → origin
    expect(repo.remoteDefaultName(RemoteDirection.Fetch as any)).toBe("origin");
    expect(repo.remoteDefaultName(RemoteDirection.Push as any)).toBe("origin");
  });
});
