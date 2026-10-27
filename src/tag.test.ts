import { execSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository, tagNameIsValid } from "./main.ts";

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
    const peeledTree = repo.peel(tagObjId, "Tree");
    expect(peeledTree).toBe(head.treeId);

    const peeledCommit = repo.peel(tagObjId, "Commit");
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

  test("writeTag() infers targetKind when omitted", () => {
    const head = repo.headCommit();
    const id = repo.writeTag({
      name: "inferred-kind",
      target: head.id,
      message: "inferred\n",
    });
    expect(repo.findTag(id).targetKind).toBe("commit");
  });
});

describe("Tag refs (src/tag.rs)", () => {
  const head = () => repo.headCommit();

  test("createTag() makes a lightweight tag without a message", () => {
    const ref = repo.createTag("__probe_lw2__", head().id);
    expect(ref.name).toBe("refs/tags/__probe_lw2__");
    expect(ref.target).toBe(head().id);
    repo.deleteTags(["__probe_lw2__"]);
    expect(repo.tagNames()).not.toContain("__probe_lw2__");
  });

  test("createTag() accepts a fully qualified name without double-prefixing", () => {
    const ref = repo.createTag("refs/tags/__probe_full__", head().id, {
      force: true,
    });
    expect(ref.name).toBe("refs/tags/__probe_full__");
    repo.deleteTags(["refs/tags/__probe_full__"]);
  });

  test("createTag({ message }) makes an annotated tag with a tagger", () => {
    const commit = head();
    const ref = repo.createTag("__probe_ann2__", commit.id, {
      message: "release\n",
      tagger: {
        name: "CI Bot",
        email: "ci@example.com",
        time: 1700000000,
        timezoneOffset: 0,
      },
    });

    const tag = repo.findTagByName("__probe_ann2__");
    expect(tag.id).toBe(ref.target);
    expect(tag.name).toBe("__probe_ann2__");
    expect(tag.message).toBe("release\n");
    expect(tag.targetKind).toBe("commit");
    expect(tag.targetId).toBe(commit.id);
    expect(tag.tagger()?.name).toBe("CI Bot");

    // Reachable from the tag object without going back through the repository.
    expect(tag.target().id).toBe(commit.id);
    expect(tag.peelToCommit().id).toBe(commit.id);
    expect(tag.peelToTree().id).toBe(commit.treeId);
    expect(tag.data().toString("utf8")).toContain("tag __probe_ann2__");

    repo.deleteTags(["__probe_ann2__"]);
  });

  test("findTagByName() takes short and full names, throws for lightweight tags", () => {
    const commit = head();
    repo.createTag("__probe_named__", commit.id, { message: "named\n" });
    expect(repo.findTagByName("__probe_named__").name).toBe("__probe_named__");
    expect(repo.findTagByName("refs/tags/__probe_named__").name).toBe("__probe_named__");
    expect(repo.tryFindTagByName("__probe_named__")?.name).toBe("__probe_named__");
    expect(repo.tryFindTagByName("__probe_missing__")).toBe(null);

    repo.createTag("__probe_lw3__", commit.id);
    expect(() => repo.findTagByName("__probe_lw3__")).toThrow();

    repo.deleteTags(["__probe_named__", "__probe_lw3__"]);
  });

  test("tags() and tagNames() list the tags", () => {
    repo.createTag("__probe_list__", head().id, { message: "listed\n" });
    expect(repo.tagNames()).toContain("__probe_list__");
    expect(repo.tags().some((r) => r.name === "refs/tags/__probe_list__")).toBe(true);
    repo.deleteTags(["__probe_list__"]);
    expect(repo.tagNames()).not.toContain("__probe_list__");
  });

  test("renameTag() moves the reference and deleteTags() reports deleted names", () => {
    repo.createTag("__probe_old__", head().id, { message: "renamed\n" });
    const renamed = repo.renameTag("__probe_old__", "__probe_new__");
    expect(renamed.name).toBe("refs/tags/__probe_new__");
    expect(repo.tryFindReference("refs/tags/__probe_old__")).toBe(null);
    // Only the reference is renamed: the object keeps the name it was created with.
    expect(repo.findTagByName("__probe_new__").name).toBe("__probe_old__");

    repo.createTag("__probe_keep__", head().id);
    expect(() => repo.renameTag("__probe_new__", "__probe_keep__")).toThrow(/already exists/);
    const forced = repo.renameTag("__probe_new__", "__probe_keep__", true);
    expect(forced.name).toBe("refs/tags/__probe_keep__");

    expect(repo.deleteTags(["__probe_keep__"])).toEqual(["refs/tags/__probe_keep__"]);
  });

  test("tagNameIsValid() mirrors branchNameIsValid()", () => {
    expect(tagNameIsValid("v1.0.0")).toBe(true);
    expect(tagNameIsValid("release/1.0")).toBe(true);
    expect(tagNameIsValid("bad..name")).toBe(false);
  });

  test("listTags() distinguishes annotated and lightweight tags", () => {
    const commit = head();
    repo.createTag("__probe_list_ann__", commit.id, {
      message: "listed annotated\n",
      tagger: {
        name: "CI Bot",
        email: "ci@example.com",
        time: 1700000000,
        timezoneOffset: 0,
      },
    });
    repo.createTag("__probe_list_lw__", commit.id);

    const all = repo.listTags();
    const ann = all.find((t) => t.name === "__probe_list_ann__");
    const lw = all.find((t) => t.name === "__probe_list_lw__");

    expect(ann).toBeDefined();
    expect(ann!.kind).toBe("Annotated");
    expect(ann!.target).toBe(commit.id);
    expect(ann!.targetKind).toBe("commit");
    expect(ann!.message).toBe("listed annotated\n");
    expect(ann!.tagger?.name).toBe("CI Bot");

    expect(lw).toBeDefined();
    expect(lw!.kind).toBe("Lightweight");
    expect(lw!.target).toBe(commit.id);
    expect(lw!.targetKind).toBe("commit");
    expect(lw!.message).toBe(null);
    expect(lw!.tagger).toBe(null);

    repo.deleteTags(["__probe_list_ann__", "__probe_list_lw__"]);
  });

  test("listTags(pattern) filters by name prefix", () => {
    const commit = head();
    repo.createTag("__probe_pfx_a__", commit.id, { message: "a\n" });
    repo.createTag("__probe_pfx_b__", commit.id, { message: "b\n" });

    const aOnly = repo.listTags("__probe_pfx_a__");
    expect(aOnly.map((t) => t.name)).toEqual(["__probe_pfx_a__"]);

    repo.deleteTags(["__probe_pfx_a__", "__probe_pfx_b__"]);
  });

  test("peelToTag() follows a tag-of-tag chain", () => {
    const commit = head();
    const tagAId = repo.writeTag({
      name: "__probe_chain_a__",
      target: commit.id,
      targetKind: "commit",
      message: "A\n",
    });
    repo.createTag("__probe_chain_b__", tagAId, { message: "B\n" });

    const tagB = repo.findTagByName("__probe_chain_b__");
    const peeled = tagB.peelToTag();
    expect(peeled.id).toBe(tagAId);
    expect(peeled.name).toBe("__probe_chain_a__");

    // A points at a commit, so peeling further throws.
    expect(() => peeled.peelToTag()).toThrow();

    repo.deleteTags(["__probe_chain_a__", "__probe_chain_b__"]);
  });
});
