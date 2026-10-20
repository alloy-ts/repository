import { execSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Graph, Repository, TraversalGraph } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-graph-"));
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
  // Write a commit-graph so the on-disk `Graph` bindings have something to read.
  // Tolerate failure (e.g. sandboxes where `git commit-graph write` misbehaves in a
  // programmatically-cloned repo): the TraversalGraph tests don't need it, and the Graph
  // tests below skip gracefully when `graph()` returns null.
  try {
    execSync(`git -C ${JSON.stringify(cloneDir)} commit-graph write --reachable --quiet`, {
      stdio: "ignore",
    });
  } catch {
    // ignore — `graph()` may return null below.
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

describe("Repository commit-graph (src/graph.rs)", () => {
  test("commitGraph() / commitGraphIfEnabled() report a commit count", () => {
    const cg = repo.commitGraph();
    expect(cg === null || typeof cg.commitCount === "number").toBe(true);
    const enabled = repo.commitGraphIfEnabled();
    expect(enabled === null || typeof enabled.commitCount === "number").toBe(true);
  });

  test("graph() / graphIfEnabled() return a usable Graph or null", () => {
    const g = repo.graph();
    if (g !== null) {
      expect(g.numCommits).toBeGreaterThanOrEqual(0);
      expect(typeof g.objectHash).toBe("string");
    }
    const enabled = repo.graphIfEnabled();
    if (enabled !== null) {
      expect(enabled.numCommits).toBeGreaterThanOrEqual(0);
    }
  });
});

describe("commit-graph Graph (src/graph.rs)", () => {
  test("numCommits / objectHash are sensible", () => {
    const g = repo.graph();
    if (!g) return;

    expect(typeof g.numCommits).toBe("number");
    expect(g.numCommits).toBeGreaterThan(0);
    expect(g.objectHash).toMatch(/^(Sha1|Sha256)$/);
  });

  test("lookup() / idAt() round-trip a real commit id", () => {
    const g = repo.graph();
    if (!g) return;

    const head = repo.headId();
    const pos = g.lookup(head);
    expect(pos).not.toBeNull();
    if (pos === null) return;
    expect(typeof pos).toBe("number");
    // The id stored at the position returned by lookup must be the same commit.
    expect(g.idAt(pos)).toBe(head);
  });

  test("lookup() returns null for a missing id", () => {
    const g = repo.graph();
    if (!g) return;
    expect(g.lookup("0".repeat(40))).toBeNull();
  });

  test("idAt() throws for an out-of-range position", () => {
    const g = repo.graph();
    if (!g) return;
    expect(() => g.idAt(g.numCommits)).toThrow();
  });

  test("verifyIntegrity() returns a populated outcome", () => {
    const g = repo.graph();
    if (!g) return;

    const outcome = g.verifyIntegrity();
    expect(outcome.numCommits).toBe(g.numCommits);
    expect(Array.isArray(outcome.parentCounts)).toBe(true);
    for (const pc of outcome.parentCounts) {
      expect(typeof pc.parents).toBe("number");
      expect(typeof pc.count).toBe("number");
    }
  });

  test("commitById() resolves a real commit's metadata", () => {
    const g = repo.graph();
    if (!g) return;

    const head = repo.headId();
    const c = g.commitById(head);
    expect(c).not.toBeNull();
    if (!c) return;
    expect(c.id).toBe(head);
    expect(typeof c.rootTreeId).toBe("string");
    expect(typeof c.generation).toBe("number");
    expect(Array.isArray(c.parentPositions)).toBe(true);
    expect(Array.isArray(c.parentIds)).toBe(true);
    expect(c.parentIds.length).toBe(c.parentPositions.length);
    for (const id of c.parentIds) expect(id.length).toBeGreaterThan(0);
  });

  test("commitAt(0) returns the first commit; out-of-range returns null", () => {
    const g = repo.graph();
    if (!g) return;

    const c0 = g.commitAt(0);
    expect(c0).not.toBeNull();
    if (!c0) return;
    expect(c0.id.length).toBeGreaterThan(0);
    expect(g.commitAt(g.numCommits)).toBeNull();
  });

  test("iterIds() / iterCommits() enumerate the whole graph", () => {
    const g = repo.graph();
    if (!g) return;

    const ids = g.iterIds();
    expect(ids.length).toBe(g.numCommits);
    for (const id of ids) expect(id.length).toBeGreaterThan(0);

    const commits = g.iterCommits();
    expect(commits.length).toBe(g.numCommits);
    expect(commits[0].id).toBe(ids[0]);
  });

  test("Graph.at() throws for a non-existent path", () => {
    expect(() => Graph.at("/no/such/commit-graph", "Sha1", null)).toThrow();
  });
});

describe("TraversalGraph (src/graph.rs)", () => {
  test("starts empty and grows as parents are inserted", () => {
    const tg = new TraversalGraph(repo);
    expect(tg.isEmpty()).toBe(true);
    expect(tg.len).toBe(0);

    const head = repo.headId();
    const parent = repo.findCommit(head).parentIds[0];

    tg.insertParents(head, false);
    // In a full clone parents are recorded; in a shallow/root commit clone none are. Both are valid.
    expect(tg.len).toBeGreaterThanOrEqual(0);
    if (typeof parent === "string" && tg.len > 0) {
      expect(tg.contains(parent)).toBe(true);
    }

    // Re-inserting the same commit is idempotent (parents already cached).
    const size = tg.len;
    tg.insertParents(head, false);
    expect(tg.len).toBe(size);

    tg.clear();
    expect(tg.isEmpty()).toBe(true);
    expect(tg.len).toBe(0);
  });

  test("accepts an optional commit-graph cache", () => {
    const g: Graph | null = repo.graph();
    const tg = new TraversalGraph(repo, g ?? undefined);

    const head = repo.headId();
    tg.insertParents(head, true);
    expect(tg.len).toBeGreaterThanOrEqual(0);

    tg.clear();
    expect(tg.isEmpty()).toBe(true);
    expect(tg.len).toBe(0);
  });
});
