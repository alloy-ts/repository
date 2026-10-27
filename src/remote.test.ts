import { execSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { RemoteDirection, RemoteProgress, Repository } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-remote-"));
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
    const r = repo.findDefaultRemote(RemoteDirection.Fetch);
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
    expect(repo.remoteDefaultName(RemoteDirection.Fetch)).toBe("origin");
    expect(repo.remoteDefaultName(RemoteDirection.Push)).toBe("origin");
  });
});

describe("Remote.handshake", () => {
  test("handshake() returns the negotiated protocol version and capabilities", async () => {
    const remote = repo.findRemote("origin");
    const hs = await remote.handshake();
    expect(["V0", "V1", "V2"]).toContain(hs.serverProtocolVersion);
    expect(Array.isArray(hs.capabilities)).toBe(true);
    // A compliant server (even the local file transport) advertises capabilities.
    expect(hs.capabilities.length).toBeGreaterThan(0);
    for (const cap of hs.capabilities) {
      expect(typeof cap.name).toBe("string");
      if (typeof cap.value === "string") {
        expect(cap.value.length).toBeGreaterThan(0);
      }
    }
    // V1 carries refs inline in the handshake; V2 does not (use a ref-map instead).
    if (hs.refs) {
      expect(Array.isArray(hs.refs)).toBe(true);
      for (const r of hs.refs) {
        expect(typeof r.name).toBe("string");
        expect(["direct", "peeled", "symbolic", "unborn"]).toContain(r.kind);
        if (typeof r.objectId === "string") {
          expect(r.objectId.length).toBeGreaterThan(0);
        }
        if (typeof r.target === "string") {
          expect(r.target.length).toBeGreaterThan(0);
        }
        if (typeof r.peeledObjectId === "string") {
          expect(r.peeledObjectId.length).toBeGreaterThan(0);
        }
      }
    }
  });

  test("handshake() accepts an explicit RemoteDirection", async () => {
    const remote = repo.findRemote("origin");
    const hs = await remote.handshake(RemoteDirection.Fetch);
    expect(typeof hs.serverProtocolVersion).toBe("string");
    expect(Array.isArray(hs.capabilities)).toBe(true);
  });
});

describe("Remote.fetch", () => {
  test("fetch() runs against the local origin and reports outcome flags", async () => {
    const remote = repo.findRemote("origin");
    const outcome = await remote.fetch();
    // Against an up-to-date local clone no pack is received, but the call must
    // complete and return the documented flags.
    expect(typeof outcome.status).toBe("string");
    expect(typeof outcome.packReceived).toBe("boolean");
    expect(typeof outcome.dryRun).toBe("boolean");
    expect(typeof outcome.negotiated).toBe("boolean");
    expect(Array.isArray(outcome.updatedRefs)).toBe(true);
  });

  test("fetch() defaultBranch() discovers the advertised HEAD target", async () => {
    const remote = repo.findRemote("origin");
    const branch = await remote.defaultBranch();
    expect(typeof branch === "string" || branch === null).toBe(true);
  });
});

describe("Remote.refMap", () => {
  test("refMap() discovers the advertised refs and mappings", async () => {
    const remote = repo.findRemote("origin");
    const map = await remote.refMap();
    expect(Array.isArray(map.remoteRefs)).toBe(true);
    // The local clone advertises at least HEAD and the checked-out branch.
    expect(map.remoteRefs.length).toBeGreaterThan(0);
    for (const r of map.remoteRefs) {
      expect(typeof r.name).toBe("string");
      expect(["direct", "peeled", "symbolic", "unborn"]).toContain(r.kind);
      if (typeof r.objectId === "string") {
        expect(r.objectId.length).toBeGreaterThan(0);
      }
    }
    expect(Array.isArray(map.mappings)).toBe(true);
    for (const m of map.mappings) {
      expect(typeof m.specIndex).toBe("number");
      expect(["explicit", "implicit"]).toContain(m.specIndexKind);
      // Each mapping points at an object id, either directly or via a matched ref.
      expect(typeof m.remoteObjectId === "string" || m.remote !== null).toBe(true);
      if (m.local !== null && m.local !== undefined) {
        expect(typeof m.local).toBe("string");
      }
    }
  });

  test("refMap() accepts an explicit RemoteDirection", async () => {
    const remote = repo.findRemote("origin");
    const map = await remote.refMap(RemoteDirection.Fetch);
    expect(Array.isArray(map.remoteRefs)).toBe(true);
  });
});

describe("RemoteProgress.fromBytes", () => {
  test("parses an action with percent, step and max", () => {
    const p = RemoteProgress.fromBytes("Receiving objects:  42% (21/50)");
    expect(p).not.toBeNull();
    expect(p!.action).toBe("Receiving objects");
    expect(p!.percent).toBe(42);
    expect(p!.step).toBe(21);
    expect(p!.max).toBe(50);
  });

  test("parses an action with step/max but no percent", () => {
    const p = RemoteProgress.fromBytes("Resolving deltas: 21/50");
    expect(p).not.toBeNull();
    expect(p!.action).toBe("Resolving deltas");
    expect(p!.percent).toBeNull();
    expect(p!.step).toBe(21);
    expect(p!.max).toBe(50);
  });

  test("returns null for a line without progress information", () => {
    // No colon → no action prefix → nothing to parse.
    expect(RemoteProgress.fromBytes("some keep-alive text")).toBeNull();
  });
});
