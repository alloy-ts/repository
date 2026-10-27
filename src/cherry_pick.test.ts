import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

// Clean cherry-pick history on its own branch:
//   cBase  file.txt = "v1"
//   cPick  file.txt = "v2"            (the commit we cherry-pick)
//   cOther file.txt = "v1", other.txt (sibling; HEAD lands here so the pick is non-trivial)
let cBase: string;
let cPick: string;
let cOther: string;

// Conflict history: cMod changes line2, cOurs also changes line2 differently.
let cBaseC: string;
let cMod: string;
let cOursC: string;

// Mainline history: a merge commit whose parents are cMA and cMB.
let cMA: string;
let cMerge: string;

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-cherry-pick-"));
  cloneDir = join(base, "clone");
  repo = await Repository.clone(realRoot, cloneDir, false);

  const bFile = repo.writeBlob(Buffer.from("v1\n"));
  const bFile2 = repo.writeBlob(Buffer.from("v2\n"));
  const bOther = repo.writeBlob(Buffer.from("other\n"));
  const tBase = repo.writeTree([
    {
      path: "file.txt",
      id: bFile,
      mode: "100644",
    },
  ]);
  cBase = repo.commit("refs/heads/cp-base", "base", tBase, []);
  const tPick = repo.writeTree([
    {
      path: "file.txt",
      id: bFile2,
      mode: "100644",
    },
  ]);
  cPick = repo.commit(
    "refs/heads/cp-pick",
    "pick\n\nChange-Id: I1234567890abcdef1234567890abcdef12345678\n",
    tPick,
    [cBase],
  );
  const tOther = repo.writeTree([
    { path: "file.txt", id: bFile, mode: "100644" },
    { path: "other.txt", id: bOther, mode: "100644" },
  ]);
  cOther = repo.commit("refs/heads/cp-other", "other", tOther, [cBase]);

  // Conflict chain.
  const bBase = repo.writeBlob(Buffer.from("line1\nline2\nline3\n"));
  const bMod = repo.writeBlob(Buffer.from("line1\nline2-modified\nline3\n"));
  const bOurs = repo.writeBlob(Buffer.from("line1\nline2-ours\nline3\n"));
  const tBaseC = repo.writeTree([
    {
      path: "file.txt",
      id: bBase,
      mode: "100644",
    },
  ]);
  cBaseC = repo.commit("refs/heads/cp-base-conflict", "base", tBaseC, []);
  const tMod = repo.writeTree([{ path: "file.txt", id: bMod, mode: "100644" }]);
  cMod = repo.commit("refs/heads/cp-pick-conflict", "modify line2", tMod, [cBaseC]);
  const tOursC = repo.writeTree([
    {
      path: "file.txt",
      id: bOurs,
      mode: "100644",
    },
  ]);
  cOursC = repo.commit("refs/heads/cp-ours-conflict", "ours line2", tOursC, [cBaseC]);

  // Mainline chain: a merge commit with parents [cMA, cMB].
  const tMBase = repo.writeTree([
    {
      path: "file.txt",
      id: bFile,
      mode: "100644",
    },
  ]);
  const cMBase = repo.commit("refs/heads/cp-ml-base", "ml-base", tMBase, []);
  const tMA = repo.writeTree([
    {
      path: "file.txt",
      id: bFile2,
      mode: "100644",
    },
  ]);
  cMA = repo.commit("refs/heads/cp-ml-a", "ml-a", tMA, [cMBase]);
  const tMB = repo.writeTree([{ path: "file.txt", id: bBase, mode: "100644" }]);
  repo.commit("refs/heads/cp-ml-b", "ml-b", tMB, [cMBase]);
  const tMerge = repo.writeTree([
    {
      path: "file.txt",
      id: bFile2,
      mode: "100644",
    },
  ]);
  cMerge = repo.commit("refs/heads/cp-ml-merge", "merge commit", tMerge, [cMA, cMBase]);
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true });
});

describe("Repository cherryPick (src/cherry_pick.rs)", () => {
  test("applies a clean change and transforms the message", () => {
    repo.setHead("refs/heads/cp-other");
    const res = repo.cherryPick(cPick);

    expect(res.commitId).toMatch(/^[0-9a-f]{40}$/);
    expect(res.conflicted).toBe(false);
    expect(res.empty).toBe(false);
    expect(res.treeId).toMatch(/^[0-9a-f]{40}$/);
    expect(res.conflicts).toHaveLength(0);
    expect(res.paths).toHaveLength(0);

    // The new commit is the HEAD tip; its message has the Change-Id stripped and a
    // "(cherry picked from commit …)" reference appended.
    const head = repo.log()[0];
    expect(head.subject).toBe("pick");
    expect(head.body).not.toContain("Change-Id:");
    expect(head.body).toContain("(cherry picked from commit");
    expect(head.body).toContain(cPick);
  });

  test("custom message override is used verbatim (no transform)", () => {
    repo.setHead("refs/heads/cp-other");
    const res = repo.cherryPick(cPick, { message: "my custom message" });

    expect(res.commitId).toMatch(/^[0-9a-f]{40}$/);
    const head = repo.log()[0];
    expect(head.subject).toBe("my custom message");
    expect(head.body).not.toContain("(cherry picked from commit");
  });

  test("noCommit applies to the index without committing", () => {
    repo.setHead("refs/heads/cp-other");
    const before = repo.headCommit().id;
    const res = repo.cherryPick(cPick, { noCommit: true });

    expect(res.commitId).toBeNull();
    expect(res.conflicted).toBe(false);
    expect(res.empty).toBe(false);
    expect(res.treeId).toMatch(/^[0-9a-f]{40}$/);
    expect(res.paths).toHaveLength(0);
    // HEAD is unchanged because no commit was created.
    expect(repo.headCommit().id).toBe(before);
  });

  test("conflict returns the conflict list and does not commit", () => {
    repo.setHead("refs/heads/cp-ours-conflict");
    const res = repo.cherryPick(cMod);

    expect(res.commitId).toBeNull();
    expect(res.conflicted).toBe(true);
    expect(res.conflicts.length).toBeGreaterThanOrEqual(1);
    const conflict = res.conflicts[0];
    expect(conflict.unresolved).toBe(true);
    expect(conflict.ours.location).toBe("file.txt");
    expect(res.paths.length).toBe(res.conflicts.length);
    expect(res.paths[0]).toBe("file.txt");
  });

  test("mergeOptions are accepted", () => {
    repo.setHead("refs/heads/cp-other");
    const res = repo.cherryPick(cPick, { mergeOptions: {} });
    expect(res.commitId).toMatch(/^[0-9a-f]{40}$/);
    expect(res.conflicted).toBe(false);
  });

  test("mainline selects the merge base and is required for merge commits", () => {
    repo.setHead("refs/heads/cp-ml-a");
    // parent[0] = cMA (ours), parent[1] = cMBase. With mainline=2 the ancestor is cMBase,
    // so the bBase -> bFile2 change applies cleanly onto cMA (which already has bFile2).
    const res = repo.cherryPick(cMerge, { mainline: 2 });
    expect(res.commitId).toMatch(/^[0-9a-f]{40}$/);
    expect(res.conflicted).toBe(false);

    // A merge commit requires mainline.
    expect(() => repo.cherryPick(cMerge)).toThrow();

    // mainline out of range throws.
    expect(() => repo.cherryPick(cMerge, { mainline: 5 })).toThrow();
  });

  test("recordOrigin: false suppresses the trailer but still strips Change-Id", () => {
    repo.setHead("refs/heads/cp-other");
    const res = repo.cherryPick(cPick, { recordOrigin: false });
    expect(res.commitId).toMatch(/^[0-9a-f]{40}$/);
    expect(res.empty).toBe(false);
    const head = repo.log()[0];
    expect(head.body).not.toContain("Change-Id:");
    expect(head.body).not.toContain("(cherry picked from commit");
  });

  test("author override sets the new commit's author", () => {
    repo.setHead("refs/heads/cp-other");
    const res = repo.cherryPick(cPick, {
      author: {
        name: "Override Author",
        email: "author@example.com",
        time: 0,
        timezoneOffset: 0,
      },
    });
    expect(res.commitId).toMatch(/^[0-9a-f]{40}$/);
    const head = repo.log()[0];
    expect(head.author_name).toBe("Override Author");
    expect(head.author_email).toBe("author@example.com");
  });

  test("committer override sets the new commit's committer", () => {
    repo.setHead("refs/heads/cp-other");
    const res = repo.cherryPick(cPick, {
      committer: {
        name: "Override Committer",
        email: "committer@example.com",
        time: 0,
        timezoneOffset: 0,
      },
    });
    expect(res.commitId).toMatch(/^[0-9a-f]{40}$/);
    const head = repo.log()[0];
    expect(head.committer_name).toBe("Override Committer");
    expect(head.committer_email).toBe("committer@example.com");
  });

  test("empty pick is reported and does not commit by default", () => {
    // cMA already contains file.txt = "v2", the exact change introduced by cPick,
    // so the cherry-pick is a no-op (merged tree equals HEAD's tree).
    repo.setHead("refs/heads/cp-ml-a");
    const before = repo.headCommit().id;
    const res = repo.cherryPick(cPick);
    expect(res.empty).toBe(true);
    expect(res.conflicted).toBe(false);
    expect(res.commitId).toBeNull();
    expect(repo.headCommit().id).toBe(before);
  });

  test("allowEmpty: true still creates a commit for an empty pick", () => {
    repo.setHead("refs/heads/cp-ml-a");
    const res = repo.cherryPick(cPick, { allowEmpty: true });
    expect(res.empty).toBe(true);
    expect(res.commitId).toMatch(/^[0-9a-f]{40}$/);
  });
});
