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
  base = mkdtempSync(join(tmpdir(), "repo-reference-"));
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

describe("Repository HEAD (src/reference.rs)", () => {
  test("head() / headId() / headCommit() / headName() agree", () => {
    const head = repo.head();
    const id = repo.headId();
    const commit = repo.headCommit();

    expect(id).toMatch(/^[0-9a-f]{40}$/);
    expect(commit.id).toBe(id);
    if (head.referent) {
      expect(head.referent.startsWith("refs/heads")).toBe(true);
    }

    const name = repo.headName();
    if (name) expect(name.startsWith("refs/heads")).toBe(true);
  });

  test("headTreeId() / headTree() / headRef() resolve HEAD", () => {
    const treeId = repo.headTreeId();
    expect(treeId).toMatch(/^[0-9a-f]{40}$/);
    expect(repo.headTree().id).toBe(treeId);

    const headRef = repo.headRef();
    if (headRef) expect(headRef.name.startsWith("refs/heads/")).toBe(true);
  });

  test("headTreeIdOrEmpty() returns a tree id even when resolvable", () => {
    expect(repo.headTreeIdOrEmpty()).toMatch(/^[0-9a-f]{40}$/);
  });

  test("setHead() points HEAD at a commit and can be restored", () => {
    const originalName = repo.headName();
    const id = repo.headId();

    repo.setHead(id);
    expect(repo.headId()).toBe(id);
    // HEAD is now detached: no symbolic name and no branch ref.
    expect(repo.headName()).toBe(null);
    expect(repo.headRef()).toBe(null);

    // Restore the original symbolic branch (if there was one).
    if (originalName) {
      repo.editReference({
        name: "HEAD",
        newTarget: originalName,
        force: true,
      });
      expect(repo.headName()).toBe(originalName);
    }
  });
});

describe("Repository references (src/reference.rs)", () => {
  test("references() lists branches/tags; HEAD is reachable via findReference", () => {
    const names = repo.references().map((r) => r.name);
    expect(Array.isArray(names)).toBe(true);
    expect(names.some((n) => n.startsWith("refs/heads/"))).toBe(true);
    expect(repo.findReference("HEAD").name).toBe("HEAD");
  });

  test("findReference finds HEAD", () => {
    expect(repo.findReference("HEAD").name).toBe("HEAD");
  });

  test("findReference throws on a missing ref", () => {
    expect(() => repo.findReference("refs/heads/__nope__")).toThrow();
  });

  test("tryFindReference() returns null instead of throwing for a missing ref", () => {
    expect(repo.tryFindReference("refs/heads/__nope__")).toBe(null);
    expect(repo.tryFindReference("HEAD")?.name).toBe("HEAD");
  });

  test("createReference / createLightweightTag / createTag and cleanup", () => {
    const head = repo.headId();

    const branch = repo.createReference("refs/heads/__probe_branch__", head, true);
    expect(branch.name).toBe("refs/heads/__probe_branch__");
    expect(repo.findReference("refs/heads/__probe_branch__").name).toBe(
      "refs/heads/__probe_branch__",
    );

    const lw = repo.createLightweightTag("__probe_lw__", head, true);
    expect(lw.name).toBe("refs/tags/__probe_lw__");

    const ann = repo.createTag("__probe_ann__", head, "annotated message", true);
    expect(ann.name).toBe("refs/tags/__probe_ann__");
    expect(repo.findTag(ann.target!).name).toBe("__probe_ann__");
    expect(repo.findTag(ann.target!).message).toContain("annotated message");

    expect(repo.references().some((r) => r.name === "refs/tags/__probe_lw__")).toBe(true);

    repo.findReference("refs/heads/__probe_branch__").delete();
    repo.findReference("refs/tags/__probe_lw__").delete();
    repo.findReference("refs/tags/__probe_ann__").delete();
  });

  test("tagReference() creates a lightweight tag", () => {
    const head = repo.headId();
    const ref = repo.tagReference("__probe_tagref__", head, false);
    expect(ref.name).toBe("refs/tags/__probe_tagref__");
    expect(repo.tryFindReference("refs/tags/__probe_tagref__")?.name).toBe(
      "refs/tags/__probe_tagref__",
    );
    repo.findReference("refs/tags/__probe_tagref__").delete();
  });

  test("editReference() creates a reference pointing at HEAD", () => {
    const head = repo.headCommit();
    const names = repo.editReference({
      name: "refs/heads/__probe_b2__",
      newTarget: head.id,
      force: true,
      logMessage: "probe",
    });
    expect(Array.isArray(names)).toBe(true);
    expect(names).toContain("refs/heads/__probe_b2__");

    const ref = repo.findReference("refs/heads/__probe_b2__");
    expect(ref.target).toBe(head.id);

    ref.delete();
    expect(() => repo.findReference("refs/heads/__probe_b2__")).toThrow();
  });

  test("editReferences() applies multiple edits", () => {
    const head = repo.headCommit();
    const names = repo.editReferences([
      { name: "refs/heads/__probe_b2_a__", newTarget: head.id, force: true },
      { name: "refs/heads/__probe_b2_b__", newTarget: head.id, force: true },
    ]);
    expect(names).toContain("refs/heads/__probe_b2_a__");
    expect(names).toContain("refs/heads/__probe_b2_b__");

    repo.findReference("refs/heads/__probe_b2_a__").delete();
    repo.findReference("refs/heads/__probe_b2_b__").delete();
  });

  test("editReferencesAs() applies edits with a committer identity", () => {
    const head = repo.headId();
    const names = repo.editReferencesAs(
      [{ name: "refs/heads/__probe_as__", newTarget: head, force: true }],
      { name: "CI Bot", email: "ci@example.com", time: 0, timezoneOffset: 0 },
    );
    expect(names).toContain("refs/heads/__probe_as__");
    repo.findReference("refs/heads/__probe_as__").delete();
  });

  test("references() lists branches/tags; HEAD is reachable via findReference", () => {
    const names = repo.references().map((r) => r.name);
    expect(Array.isArray(names)).toBe(true);
    expect(names.some((n) => n.startsWith("refs/heads/"))).toBe(true);
    expect(repo.findReference("HEAD").name).toBe("HEAD");
  });

  test("findReference finds HEAD", () => {
    expect(repo.findReference("HEAD").name).toBe("HEAD");
  });

  test("findReference throws on a missing ref", () => {
    expect(() => repo.findReference("refs/heads/__nope__")).toThrow();
  });

  test("createReference / createLightweightTag / createTag and cleanup", () => {
    const head = repo.headId();

    const branch = repo.createReference("refs/heads/__probe_branch__", head, true);
    expect(branch.name).toBe("refs/heads/__probe_branch__");
    expect(repo.findReference("refs/heads/__probe_branch__").name).toBe(
      "refs/heads/__probe_branch__",
    );

    const lw = repo.createLightweightTag("__probe_lw__", head, true);
    expect(lw.name).toBe("refs/tags/__probe_lw__");

    const ann = repo.createTag("__probe_ann__", head, "annotated message", true);
    expect(ann.name).toBe("refs/tags/__probe_ann__");
    expect(repo.findTag(ann.target!).name).toBe("__probe_ann__");
    expect(repo.findTag(ann.target!).message).toContain("annotated message");

    expect(repo.references().some((r) => r.name === "refs/tags/__probe_lw__")).toBe(true);

    repo.findReference("refs/heads/__probe_branch__").delete();
    repo.findReference("refs/tags/__probe_lw__").delete();
    repo.findReference("refs/tags/__probe_ann__").delete();
  });

  test("tryFindReference() returns null instead of throwing for a missing ref", () => {
    expect(repo.tryFindReference("refs/heads/__nope__")).toBe(null);
    expect(repo.tryFindReference("HEAD")?.name).toBe("HEAD");
  });

  test("tagReference() creates a lightweight tag", () => {
    const head = repo.headId();
    const ref = repo.tagReference("__probe_tagref__", head, false);
    expect(ref.name).toBe("refs/tags/__probe_tagref__");
    expect(repo.tryFindReference("refs/tags/__probe_tagref__")?.name).toBe(
      "refs/tags/__probe_tagref__",
    );
    repo.findReference("refs/tags/__probe_tagref__").delete();
  });

  test("editReferencesAs() applies edits with a committer identity", () => {
    const head = repo.headId();
    const names = repo.editReferencesAs(
      [{ name: "refs/heads/__probe_as__", newTarget: head, force: true }],
      { name: "CI Bot", email: "ci@example.com", time: 0, timezoneOffset: 0 },
    );
    expect(names).toContain("refs/heads/__probe_as__");
    repo.findReference("refs/heads/__probe_as__").delete();
  });
});
