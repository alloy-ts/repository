import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository } from "./main.ts";

let fixture: string;
let repo: Repository;
// A shared base commit with two divergent branches (clean merge), plus a
// conflicting pair that both edit the same line of `base.txt`.
let baseId: string;
let mainTip: string;
let featureTip: string;
let conflictMain: string;
let conflictFeature: string;
let baseTree: string;
let mainTree: string;
let featureTree: string;
let conflictMainTree: string;
let conflictFeatureTree: string;

function git(args: string) {
  execSync(`git ${args}`, { cwd: fixture, stdio: "ignore" });
}

function rev(spec: string): string {
  return execSync(`git rev-parse ${spec}`, { cwd: fixture }).toString().trim();
}

function commitFile(name: string, content: string, message: string) {
  writeFileSync(join(fixture, name), content);
  git(`add ${name}`);
  git(`commit -m ${JSON.stringify(message)}`);
}

beforeAll(() => {
  fixture = mkdtempSync(join(tmpdir(), "repo-merge-"));
  git("init --initial-branch=main");
  git("config user.email test@example.com");
  git("config user.name Test");
  git("config commit.gpgsign false");

  // Shared ancestor.
  commitFile("base.txt", "line1\nline2\nline3\n", "base");
  baseId = rev("HEAD");

  // main: add a new file (no overlap with feature).
  commitFile("main-only.txt", "from main\n", "main change");
  mainTip = rev("HEAD");

  // feature branches off the shared base and adds its own file.
  git(`checkout -b feature ${baseId}`);
  commitFile("feature-only.txt", "from feature\n", "feature change");
  featureTip = rev("HEAD");
  git("checkout main");

  // Conflicting pair: both edit line2 of base.txt differently.
  git(`checkout -b conflict-main ${baseId}`);
  commitFile("base.txt", "line1\nMAIN\nline3\n", "conflict main edit");
  conflictMain = rev("HEAD");
  git(`checkout -b conflict-feature ${baseId}`);
  commitFile("base.txt", "line1\nFEATURE\nline3\n", "conflict feature edit");
  conflictFeature = rev("HEAD");
  git("checkout main");

  baseTree = rev(`${baseId}^{tree}`);
  mainTree = rev(`${mainTip}^{tree}`);
  featureTree = rev(`${featureTip}^{tree}`);
  conflictMainTree = rev(`${conflictMain}^{tree}`);
  conflictFeatureTree = rev(`${conflictFeature}^{tree}`);

  repo = Repository.open(fixture);
});

afterAll(() => {
  rmSync(fixture, { recursive: true, force: true });
});

describe("Repository merge (src/merge.rs)", () => {
  test("blobMergeOptions() reports the text-driver configuration", () => {
    const opts = repo.blobMergeOptions();
    expect(typeof opts.isVirtualAncestor).toBe("boolean");
    expect(typeof opts.diffAlgorithm).toBe("string");
    expect(typeof opts.conflict).toBe("string");
  });

  test("mergeBase() finds the shared ancestor of two divergent commits", () => {
    expect(repo.mergeBase(mainTip, featureTip)).toBe(baseId);
    // symmetric
    expect(repo.mergeBase(featureTip, mainTip)).toBe(baseId);
    // a commit and itself is its own merge base
    expect(repo.mergeBase(mainTip, mainTip)).toBe(mainTip);
  });

  test("mergeBaseWithGraph() matches mergeBase()", () => {
    expect(repo.mergeBaseWithGraph(mainTip, featureTip)).toBe(baseId);
  });

  test("mergeBasesMany() / mergeBasesManyWithGraph() return the single base", () => {
    expect(repo.mergeBasesMany(mainTip, [featureTip])).toEqual([baseId]);
    expect(repo.mergeBasesManyWithGraph(mainTip, [featureTip])).toEqual([baseId]);
  });

  test("mergeBaseOctopus() / mergeBaseOctopusWithGraph() find the common base", () => {
    expect(repo.mergeBaseOctopus([mainTip, featureTip])).toBe(baseId);
    expect(repo.mergeBaseOctopusWithGraph([mainTip, featureTip])).toBe(baseId);
  });

  test("mergeCommits() merges non-overlapping changes cleanly", () => {
    const outcome = repo.mergeCommits(mainTip, featureTip);
    expect(outcome.conflicts).toBe(false);
    expect(outcome.treeId).toMatch(/^[0-9a-f]{40}$/);

    // The merged tree carries both sides' files plus the untouched base file.
    const tree = repo.findTree(outcome.treeId);
    expect(tree.entry("base.txt")).not.toBeNull();
    expect(tree.entry("main-only.txt")).not.toBeNull();
    expect(tree.entry("feature-only.txt")).not.toBeNull();
  });

  test("mergeCommits() reports conflicts for overlapping edits", () => {
    const outcome = repo.mergeCommits(conflictMain, conflictFeature);
    expect(outcome.conflicts).toBe(true);
  });

  test("mergeTrees() merges three trees cleanly", () => {
    const outcome = repo.mergeTrees(mainTree, featureTree, baseTree);
    expect(outcome.conflicts).toBe(false);
    expect(outcome.treeId).toMatch(/^[0-9a-f]{40}$/);

    const tree = repo.findTree(outcome.treeId);
    expect(tree.entry("main-only.txt")).not.toBeNull();
    expect(tree.entry("feature-only.txt")).not.toBeNull();
  });

  test("mergeTrees() reports conflicts for overlapping edits", () => {
    const outcome = repo.mergeTrees(conflictMainTree, conflictFeatureTree, baseTree);
    expect(outcome.conflicts).toBe(true);
  });

  test("virtualMergeBase() returns a single base directly (no recursion)", () => {
    const outcome = repo.virtualMergeBase([mainTip]);
    expect(outcome.virtualMergeBases).toEqual([]);
    expect(outcome.commitId).toBe(mainTip);
    expect(outcome.treeId).toBe(mainTree);
  });

  test("virtualMergeBaseWithGraph() matches virtualMergeBase()", () => {
    const outcome = repo.virtualMergeBaseWithGraph([mainTip]);
    expect(outcome.virtualMergeBases).toEqual([]);
    expect(outcome.commitId).toBe(mainTip);
    expect(outcome.treeId).toBe(mainTree);
  });

  test("mergeBaseWithGraph / mergeBasesMany agree with mergeBase", () => {
    const head = repo.headCommit();
    if (head.parentIds.length > 0 && repo.objectExists(head.parentIds[0])) {
      const parent = head.parentIds[0];
      expect(repo.mergeBaseWithGraph(head.id, parent)).toBe(parent);
      expect(repo.mergeBasesMany(head.id, [parent])).toContain(parent);
      expect(repo.mergeBasesManyWithGraph(head.id, [parent])).toContain(parent);
    }
  });

  test("mergeBaseOctopusWithGraph matches mergeBaseOctopus", () => {
    const head = repo.revParseSingle("HEAD");
    expect(repo.mergeBaseOctopusWithGraph([head])).toBe(repo.mergeBaseOctopus([head]));
  });

  test("blobMergeOptions() exposes the resolved text-merge configuration", () => {
    const opts = repo.blobMergeOptions();
    expect(typeof opts.diffAlgorithm).toBe("string");
    expect(typeof opts.conflict).toBe("string");
    expect(opts.isVirtualAncestor).toBe(false);
  });

  test("virtualMergeBase(WithGraph) on a commit and its parent", () => {
    const head = repo.headCommit();
    if (head.parentIds.length > 0 && repo.objectExists(head.parentIds[0])) {
      const parent = head.parentIds[0];
      const outcome = repo.virtualMergeBase([head.id, parent]);
      // The last virtual base is the commit returned as `commitId`.
      expect(outcome.virtualMergeBases.length).toBeGreaterThanOrEqual(1);
      expect(outcome.virtualMergeBases[outcome.virtualMergeBases.length - 1]).toBe(
        outcome.commitId,
      );
      expect(outcome.commitId).toMatch(/^[0-9a-f]{40}$/);
      expect(outcome.treeId).toMatch(/^[0-9a-f]{40}$/);
      const viaGraph = repo.virtualMergeBaseWithGraph([head.id, parent]);
      expect(viaGraph.commitId).toBe(outcome.commitId);
      expect(viaGraph.treeId).toBe(outcome.treeId);
    }
  });

  test("mergeTrees() and mergeCommits() perform tree merges", () => {
    const head = repo.headCommit();
    const outcome = repo.mergeTrees(head.treeId, head.treeId, head.treeId);
    expect(typeof outcome.treeId).toBe("string");
    expect(typeof outcome.conflicts).toBe("boolean");

    const commitOutcome = repo.mergeCommits(head.id, head.id);
    expect(typeof commitOutcome.treeId).toBe("string");
    expect(typeof commitOutcome.conflicts).toBe("boolean");
  });

  test("mergeBase / mergeBaseOctopus", () => {
    const head = repo.revParseSingle("HEAD");
    const commit = repo.headCommit();
    if (commit.parentIds.length > 0 && repo.objectExists(commit.parentIds[0])) {
      const parent = commit.parentIds[0];
      expect(repo.mergeBase(head, head)).toBe(head);
      expect(repo.mergeBase(head, parent)).toBe(parent);
      expect(repo.mergeBaseOctopus([head, parent])).toBe(parent);
    } else {
      expect(repo.mergeBase(head, head)).toBe(head);
      expect(repo.mergeBaseOctopus([head])).toBe(head);
    }
  });

  test("reflects configured merge behaviour", () => {
    const opts = repo.treeMergeOptions();
    expect(typeof opts.rewritesEnabled).toBe("boolean");
    expect(opts.markerSizeMultiplier).toBeGreaterThanOrEqual(0);
    expect(typeof opts.blobMerge).toBe("string");
    expect(opts.failOnConflict).toBe(false);
  });
});
