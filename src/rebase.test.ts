import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository } from "./main.ts";

let fixture: string;
let repo: Repository;
// A shared base commit; `feature` adds two clean commits on top of it while
// `main` advances independently. `conflict` edits `base.txt` line2 (FEATURE)
// and `conflict-main` edits the same line (MAIN), so rebasing one onto the
// other produces an unresolved conflict.
let baseId: string;
let mainTip: string;
let featureA: string;
let featureTip: string;
let conflictTip: string;
let conflictMainTip: string;
let conflictTree: string;

function git(args: string) {
  execSync(`git ${args}`, { cwd: fixture, stdio: "ignore" });
}

function rev(spec: string): string {
  return execSync(`git rev-parse ${spec}`, { cwd: fixture }).toString().trim();
}

function commitFile(name: string, content: string, message: string, author?: string) {
  writeFileSync(join(fixture, name), content);
  git(`add ${name}`);
  const authorArg = author ? ` --author ${JSON.stringify(author)}` : "";
  git(`commit${authorArg} -m ${JSON.stringify(message)}`);
}

beforeAll(() => {
  fixture = mkdtempSync(join(tmpdir(), "repo-rebase-"));
  git("init --initial-branch=main");
  git("config user.email test@example.com");
  git("config user.name Test");
  git("config commit.gpgsign false");

  // Shared ancestor.
  commitFile("base.txt", "line1\nline2\nline3\n", "base");
  baseId = rev("HEAD");

  // feature: two clean commits with a distinct author on the first one.
  git(`checkout -b feature ${baseId}`);
  commitFile("feature-a.txt", "from feature A\n", "feature A", "Feature Author <feat@example.com>");
  featureA = rev("HEAD");
  commitFile("feature-b.txt", "from feature B\n", "feature B");
  featureTip = rev("HEAD");

  // main advances independently.
  git("checkout main");
  commitFile("main-only.txt", "from main\n", "main change");
  mainTip = rev("HEAD");

  // A second branch pointing at the same commits, for the one-shot rebase.
  git(`branch feature2 ${featureTip}`);

  // conflict branch: edits base.txt line2 (clashes with conflict-main).
  git(`checkout -b conflict ${baseId}`);
  commitFile("base.txt", "line1\nFEATURE\nline3\n", "conflict feature edit");
  conflictTip = rev("HEAD");
  conflictTree = rev(`${conflictTip}^{tree}`);

  git(`checkout -b conflict-main ${baseId}`);
  commitFile("base.txt", "line1\nMAIN\nline3\n", "conflict main edit");
  conflictMainTip = rev("HEAD");
  git("checkout main");

  repo = Repository.open(fixture);
});

afterAll(() => {
  rmSync(fixture, { recursive: true, force: true });
});

describe("Repository rebase (src/rebase.rs)", () => {
  test("rebaseInit() plans the commits between upstream and branch, oldest first", () => {
    const session = repo.rebaseInit("feature", baseId, mainTip);
    expect(session.len).toBe(2);
    expect(session.origHeadName).toBe("refs/heads/feature");
    expect(session.origHeadId).toBe(featureTip);
    expect(session.onto).toBe(mainTip);
    expect(session.tipId).toBe(mainTip);
    expect(session.operationCurrent()).toBeNull();

    expect(session.nth(0)).toEqual({ kind: "pick", id: featureA });
    expect(session.nth(1)).toEqual({ kind: "pick", id: featureTip });
    expect(() => session.nth(2)).toThrow();
    expect(() => session.nth(5)).toThrow();
  });

  test("rebaseInit() defaults upstream to the merge base with HEAD and onto to HEAD", () => {
    // HEAD is main; the fork point of feature and main is baseId.
    const session = repo.rebaseInit("feature");
    expect(session.len).toBe(2);
    expect(session.onto).toBe(mainTip);
    session.abort();
  });

  test("rebaseInit() has no origHeadName for a detached (commit id) branch", () => {
    const session = repo.rebaseInit(featureTip, baseId, mainTip);
    expect(session.origHeadName).toBeNull();
    expect(session.origHeadId).toBe(featureTip);
    session.abort();
  });

  test("step rebase replays commits preserving author and message, then finish moves the branch", () => {
    const session = repo.rebaseInit("feature", baseId, mainTip);

    const step1 = session.next();
    expect(step1.index).toBe(0);
    expect(step1.operation).toEqual({ kind: "pick", id: featureA });
    expect(step1.merge.conflicts).toBe(false);
    expect(session.operationCurrent()).toBe(0);
    expect(() => session.next()).toThrow(); // previous op not committed yet

    const commit1 = session.commit();
    expect(commit1).toMatch(/^[0-9a-f]{40}$/);
    // The replayed commit keeps the original author and message...
    const rebased1 = repo.findCommit(commit1);
    expect(rebased1.message.trim()).toBe("feature A");
    expect(rebased1.author().name).toBe("Feature Author");
    expect(rebased1.author().email).toBe("feat@example.com");
    // ...is parented onto `onto`...
    expect(rebased1.parentIds).toEqual([mainTip]);
    // ...and carries the feature file.
    const tree1 = repo.findTree(rebased1.treeId);
    expect(tree1.entry("main-only.txt")).not.toBeNull();
    expect(tree1.entry("feature-a.txt")).not.toBeNull();

    const step2 = session.next();
    expect(step2.index).toBe(1);
    expect(step2.operation).toEqual({ kind: "pick", id: featureTip });
    const commit2 = session.commit();
    expect(repo.findCommit(commit2).parentIds).toEqual([commit1]);

    expect(() => session.next()).toThrow(); // no more operations
    const tip = session.finish();
    expect(tip).toBe(commit2);
    // finish() moved the branch ref to the rebased tip.
    expect(rev("refs/heads/feature")).toBe(tip);
    // The rebased history is linear: main -> A' -> B'.
    expect(repo.isAncestor(mainTip, tip)).toBe(true);
    expect(repo.isAncestor(featureA, tip)).toBe(false);

    // A finished session rejects further operations.
    expect(() => session.commit()).toThrow();
  });

  test("commit() honours explicit committer and message override", () => {
    const session = repo.rebaseInit("feature2", baseId, mainTip);
    session.next();
    const id = session.commit(
      {
        name: "Rebaser",
        email: "rebaser@example.com",
        time: 1700000000,
        timezoneOffset: 0,
      },
      "reworded",
    );
    const commit = repo.findCommit(id);
    expect(commit.message.trim()).toBe("reworded");
    expect(commit.committer().name).toBe("Rebaser");
    expect(commit.committer().email).toBe("rebaser@example.com");
    // Author still comes from the original commit.
    expect(commit.author().name).toBe("Feature Author");

    session.next();
    const id2 = session.commit(); // default committer and message
    expect(repo.findCommit(id2).message.trim()).toBe("feature B");
    session.finish();
    expect(rev("refs/heads/feature2")).toBe(id2);
  });

  test("rebaseInit() on an empty plan finishes immediately at onto", () => {
    const session = repo.rebaseInit("main", mainTip, mainTip);
    expect(session.len).toBe(0);
    expect(() => session.next()).toThrow();
    expect(session.finish()).toBe(mainTip);
  });

  test("next() reports unresolved conflicts; commit() requires a resolved tree", () => {
    const session = repo.rebaseInit("conflict", baseId, conflictMainTip);
    expect(session.len).toBe(1);

    const step = session.next();
    expect(step.index).toBe(0);
    expect(step.merge.conflicts).toBe(true);
    expect(step.merge.hasUnresolvedConflicts).toBe(true);
    expect(step.merge.conflictDetails.length).toBe(1);

    // Committing without resolving throws.
    expect(() => session.commit()).toThrow();
    expect(() => session.finish()).toThrow(); // op still pending

    // Build a resolved tree: conflict tree with base.txt set to the merged content.
    const resolvedBlob = repo.writeBlob(Buffer.from("line1\nRESOLVED\nline3\n"));
    const resolvedTree = repo.editTree(conflictTree, [
      {
        path: "base.txt",
        id: resolvedBlob,
      },
    ]);
    const id = session.commit(undefined, undefined, resolvedTree);
    const rebased = repo.findCommit(id);
    expect(rebased.message.trim()).toBe("conflict feature edit");
    expect(rebased.parentIds).toEqual([conflictMainTip]);
    const text = repo.findTree(rebased.treeId).entry("base.txt");
    expect(repo.findBlob(text!.id).content().toString()).toContain("RESOLVED");

    const tip = session.finish();
    expect(rev("refs/heads/conflict")).toBe(tip);
  });

  test("abort() discards the in-memory session without touching refs", () => {
    const before = rev("refs/heads/feature2");
    const session = repo.rebaseInit("feature2", baseId, mainTip);
    session.next();
    session.abort();
    expect(() => session.commit()).toThrow();
    expect(() => session.next()).toThrow();
    expect(() => session.finish()).toThrow();
    // feature2 was finished by an earlier test; the point is that aborting a
    // new session leaves it exactly where it was.
    expect(rev("refs/heads/feature2")).toBe(before);
  });

  test("one-shot rebase() replays everything and moves the branch", () => {
    git(`branch feature3 ${featureTip}`);
    const outcome = repo.rebase("feature3", baseId, mainTip);
    expect(outcome.completed).toBe(true);
    expect(outcome.total).toBe(2);
    expect(outcome.applied).toBe(2);
    expect(outcome.conflict).toBeUndefined();

    const tip = repo.findCommit(outcome.headId);
    expect(tip.message.trim()).toBe("feature B");
    expect(tip.parentIds.length).toBe(1);
    // Walk first-parent back: B' -> A' -> main.
    const a = repo.findCommit(tip.parentIds[0]!);
    expect(a.message.trim()).toBe("feature A");
    expect(a.parentIds).toEqual([mainTip]);
    expect(rev("refs/heads/feature3")).toBe(outcome.headId);
    const tree = repo.findTree(tip.treeId);
    expect(tree.entry("main-only.txt")).not.toBeNull();
    expect(tree.entry("feature-a.txt")).not.toBeNull();
    expect(tree.entry("feature-b.txt")).not.toBeNull();
  });

  test("one-shot rebase() stops at the first unresolved conflict and leaves refs untouched", () => {
    const before = rev("refs/heads/conflict-main");
    const outcome = repo.rebase("conflict-main", baseId, conflictTip);
    expect(outcome.completed).toBe(false);
    expect(outcome.applied).toBe(0);
    expect(outcome.total).toBe(1);
    expect(outcome.headId).toBe(conflictTip);

    const failed = outcome.conflict!;
    expect(failed.index).toBe(0);
    expect(failed.operation).toEqual({ kind: "pick", id: conflictMainTip });
    expect(failed.merge.hasUnresolvedConflicts).toBe(true);
    expect(failed.merge.conflictDetails[0]!.ours.location).toBe("base.txt");

    // The branch ref was not moved.
    expect(rev("refs/heads/conflict-main")).toBe(before);
  });

  test("one-shot rebase() with nothing to do completes without changes", () => {
    const outcome = repo.rebase("main", mainTip, mainTip);
    expect(outcome.completed).toBe(true);
    expect(outcome.total).toBe(0);
    expect(outcome.applied).toBe(0);
    expect(outcome.headId).toBe(mainTip);
  });

  test("one-shot rebase() defaults upstream/onto to the fork point and HEAD", () => {
    git(`branch feature4 ${featureTip}`);
    // HEAD is main; feature4 forked at baseId.
    const outcome = repo.rebase("feature4");
    expect(outcome.completed).toBe(true);
    expect(outcome.total).toBe(2);
    expect(repo.isAncestor(mainTip, outcome.headId)).toBe(true);
    expect(rev("refs/heads/feature4")).toBe(outcome.headId);
  });
});
