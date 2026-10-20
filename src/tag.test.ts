import { execSync } from "node:child_process";
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
  base = mkdtempSync(join(tmpdir(), "repo-tag-"));
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
  rmSync(base, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 200,
  });
});

function writeTagOnNewCommit(name: string): { tagObjId: string; newCommitId: string } {
  const head = repo.headCommit();
  const newCommitId = repo.writeCommit({
    tree: head.treeId,
    parents: [head.id],
    message: "custom loose commit message\n",
  });
  const tagObjId = repo.writeTag({
    name,
    target: newCommitId,
    targetKind: "commit",
    message: "custom annotated tag\n",
  });
  return { tagObjId, newCommitId };
}

describe("Repository tags", () => {
  test("writeTag() and peel() roundtrip", () => {
    const { tagObjId, newCommitId } = writeTagOnNewCommit("v1.0.0-custom");
    expect(tagObjId).toMatch(/^[0-9a-f]{40}$/);

    const head = repo.headCommit();
    const peeledTree = repo.peel(tagObjId, "tree");
    expect(peeledTree).toBe(head.treeId);

    const peeledCommit = repo.peel(tagObjId, "commit");
    expect(peeledCommit).toBe(newCommitId);
  });

  test("findTag() decodes the written tag", () => {
    const { tagObjId, newCommitId } = writeTagOnNewCommit("v2.0.0-custom");
    const tag = repo.findTag(tagObjId);
    expect(tag.id).toBe(tagObjId);
    expect(tag.name).toBe("v2.0.0-custom");
    expect(tag.message).toBe("custom annotated tag\n");
    expect(tag.targetId).toBe(newCommitId);
    expect(tag.targetKind).toBe("commit");
  });

  test("writeTag() rejects an invalid target kind", () => {
    const head = repo.headCommit();
    expect(() =>
      repo.writeTag({
        name: "bad-kind",
        target: head.id,
        targetKind: "bogus",
        message: "should fail\n",
      }),
    ).toThrow(/invalid target_kind bogus/);
  });
});
