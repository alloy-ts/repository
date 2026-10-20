import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-conflict-"));
  cloneDir = join(base, "clone");
  repo = await Repository.clone(realRoot, cloneDir);
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true });
});

function blob(s: string): string {
  return repo.writeBlob(Buffer.from(s));
}
function tree(id: string): string {
  return repo.writeTree([{ path: "file.txt", id, mode: "100644" }]);
}

/** Write a conflicted index (stages 1/2/3) for `file.txt` into the repo. */
function makeConflict(ancestor: string, ours: string, theirs: string): void {
  const aT = tree(blob(ancestor));
  const oT = tree(blob(ours));
  const tT = tree(blob(theirs));
  repo.applyMergeConflictsToIndex(oT, tT, aT, undefined, "git", "prune", true);
}

describe("Repository conflicts (src/conflict.rs)", () => {
  test("conflicts() classifies a both-modified conflict", () => {
    makeConflict("base\n", "ours\n", "theirs\n");
    const conflicts = repo.conflicts();
    expect(conflicts.length).toBe(1);
    expect(conflicts[0].path).toBe("file.txt");
    expect(conflicts[0].kind).toBe("BothModified");
  });

  test("resolve_conflict collapses the conflict to a stage-0 entry", () => {
    makeConflict("base\n", "ours\n", "theirs\n");
    expect(repo.index().hasConflicts()).toBe(true);

    repo.resolveConflict("file.txt", Buffer.from("resolved\n"));

    // Conflict gone, and the index is no longer conflicted.
    expect(repo.conflicts().length).toBe(0);
    expect(repo.index().hasConflicts()).toBe(false);
  });

  test("conflicts_async agrees with the sync variant", async () => {
    makeConflict("base\n", "ours\n", "theirs\n");
    const sync = repo.conflicts();
    const async = await repo.conflictsAsync();
    expect(async).toEqual(sync);
  });

  test("resolve_conflict_async parity", async () => {
    makeConflict("base\n", "ours\n", "theirs\n");
    await repo.resolveConflictAsync("file.txt", Buffer.from("resolved-async\n"), undefined);
    expect(repo.conflicts().length).toBe(0);
    expect(repo.index().hasConflicts()).toBe(false);
  });

  test("classifies a both-added conflict (no ancestor)", () => {
    // Ancestor tree has no file.txt; both sides add it with different content.
    const emptyT = repo.writeTree([]);
    const oT = tree(blob("ours\n"));
    const tT = tree(blob("theirs\n"));
    repo.applyMergeConflictsToIndex(oT, tT, emptyT, undefined, "git", "prune", true);

    const conflicts = repo.conflicts();
    expect(conflicts.length).toBe(1);
    expect(conflicts[0].path).toBe("file.txt");
    expect(conflicts[0].kind).toBe("BothAdded");
  });
});
