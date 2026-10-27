import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

// Deterministic history built on its own branch inside the clone:
//   c1  file.txt = "v1"            (root commit)
//   c2  file.txt = "v2"            (modifies file.txt)
//   c3  file.txt = "v2", new.txt   (adds new.txt)
let c1: string;
let c2: string;
let c3: string;
let b1: string; // "v1" blob id
let b2: string; // "v2" blob id

// History used to force a revert conflict:
//   cBase  file.txt = "line1\nline2\nline3\n"   (root)
//   cA     file.txt = "line1\nline2-modified\nline3\n"   (reverted commit)
//   cOurs  file.txt = "line1\nline2-ours\nline3\n"        (base for the revert)
let cBase: string;
let cA: string;
let cOurs: string;
let bBase: string;
let bA: string;
let bOurs: string;

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-revert-"));
  cloneDir = join(base, "clone");
  repo = await Repository.clone(realRoot, cloneDir);

  b1 = repo.writeBlob(Buffer.from("v1\n"));
  b2 = repo.writeBlob(Buffer.from("v2\n"));
  const t1 = repo.writeTree([{ path: "file.txt", id: b1, mode: "100644" }]);
  const t2 = repo.writeTree([{ path: "file.txt", id: b2, mode: "100644" }]);
  c1 = repo.commit("refs/heads/revert-test", "c1", t1, []);
  c2 = repo.commit("refs/heads/revert-test", "c2", t2, [c1]);

  const b3 = repo.writeBlob(Buffer.from("new\n"));
  const t3 = repo.writeTree([
    { path: "file.txt", id: b2, mode: "100644" },
    { path: "new.txt", id: b3, mode: "100644" },
  ]);
  c3 = repo.commit("refs/heads/revert-test", "c3", t3, [c2]);

  // Colliding history on a separate branch.
  bBase = repo.writeBlob(Buffer.from("line1\nline2\nline3\n"));
  bA = repo.writeBlob(Buffer.from("line1\nline2-modified\nline3\n"));
  bOurs = repo.writeBlob(Buffer.from("line1\nline2-ours\nline3\n"));
  const tBase = repo.writeTree([
    {
      path: "file.txt",
      id: bBase,
      mode: "100644",
    },
  ]);
  const tA = repo.writeTree([{ path: "file.txt", id: bA, mode: "100644" }]);
  const tOurs = repo.writeTree([
    {
      path: "file.txt",
      id: bOurs,
      mode: "100644",
    },
  ]);
  cBase = repo.commit("refs/heads/revert-conflict", "base", tBase, []);
  cA = repo.commit("refs/heads/revert-conflict", "a", tA, [cBase]);
  cOurs = repo.commit("refs/heads/revert-conflict", "ours", tOurs, [cA]);
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true });
});

/** Map index entries to the fields we care about for comparisons. */
function slim(entries: { path: string; id: string; stage: number }[]) {
  return entries.map((e) => ({ path: e.path, id: e.id, stage: e.stage }));
}

describe("Repository revert (src/revert.rs)", () => {
  test("revertCommit undoes a file modification (restores v1)", () => {
    // Reverting c2 (the v1 -> v2 change) onto itself restores file.txt to v1.
    const index = repo.revertCommit({ ours: c2, revert: c2 });
    const entries = index.entries();
    expect(entries).toHaveLength(1);
    expect(entries[0].path).toBe("file.txt");
    expect(entries[0].id).toBe(b1); // the blob is back to v1
    expect(entries[0].stage).toBe(0);
  });

  test("revertCommit removes a file that was added by the reverted commit", () => {
    // c3 only added new.txt; reverting it drops new.txt and yields c2's tree
    // (file.txt still points at the v2 blob).
    const index = repo.revertCommit({ ours: c3, revert: c3 });
    const entries = index.entries();
    expect(entries).toHaveLength(1);
    expect(entries[0].path).toBe("file.txt");
    expect(entries[0].id).toBe(b2);
    expect(entries.some((e) => e.path === "new.txt")).toBe(false);
  });

  test("revert() defaults to HEAD (no worktree checkout)", () => {
    // Point HEAD at the revert-test branch (tip = c3) and revert c3 with the
    // default "ours" base. Unlike git2, this returns the reverted index instead
    // of touching the worktree.
    repo.setHead("refs/heads/revert-test");
    const index = repo.revert(c3);
    const entries = index.entries();
    expect(entries).toHaveLength(1);
    expect(entries[0].id).toBe(b2);
  });

  test("sync and async revert methods agree", async () => {
    // revertCommit / revertCommitAsync (explicit base)
    const syncCommit = slim(repo.revertCommit({ ours: c2, revert: c2 }).entries());
    const asyncCommit = slim((await repo.revertCommitAsync({ ours: c2, revert: c2 })).entries());
    expect(asyncCommit).toEqual(syncCommit);

    // revert / revertAsync (HEAD base). HEAD is at c3 from the previous test.
    repo.setHead("refs/heads/revert-test");
    const syncRevert = slim(repo.revert(c3).entries());
    const asyncRevert = slim((await repo.revertAsync(c3)).entries());
    expect(asyncRevert).toEqual(syncRevert);
  });

  test("reverting a root commit (no parents) throws", () => {
    expect(() => repo.revertCommit({ ours: c1, revert: c1 })).toThrow();
  });

  test("mainline out of range for a non-merge commit throws", () => {
    expect(() => repo.revertCommit({ ours: c2, revert: c2 }, { mainline: 2 })).toThrow();
  });

  test("revertCommit records conflict stages when the change collides", () => {
    // Reverting cA (which changed line2) onto cOurs, which also changed line2
    // differently, cannot be applied cleanly. The result is a conflicted index
    // with stages 1/2/3 (git2's revert_commit leaves the same conflict stages).
    const index = repo.revertCommit({ ours: cOurs, revert: cA });
    expect(index.hasConflicts()).toBe(true);
    const conflict = index.conflictGet("file.txt");
    expect(conflict).not.toBe(null);
    // Stage 1 (ancestor) = the reverted commit's tree (line2-modified).
    expect(conflict!.ancestor?.id).toBe(bA);
    // Stage 2 (ours) = our side (line2-ours).
    expect(conflict!.our?.id).toBe(bOurs);
    // Stage 3 (theirs) = the reverted commit's parent (line2, the original).
    expect(conflict!.theirs?.id).toBe(bBase);
  });
});
