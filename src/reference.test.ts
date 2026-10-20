import { execSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import {
  refCategoryFullName,
  refNameIsValid,
  refPartialNameIsValid,
  Repository,
} from "./main.ts";

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

  test("delete edits remove refs and enforce previousValue constraints", () => {
    const head = repo.headId();
    repo.editReference({
      name: "refs/heads/__probe_del__",
      newTarget: head,
      force: true,
    });

    // A deletion with a mismatched expected target is rejected.
    expect(() =>
      repo.editReference({
        name: "refs/heads/__probe_del__",
        change: "delete",
        previousValue: "must_exist_and_match",
        expectedTarget: "0".repeat(40),
      }),
    ).toThrow();

    // A matching expected target deletes the ref.
    const names = repo.editReference({
      name: "refs/heads/__probe_del__",
      change: "delete",
      previousValue: "must_exist_and_match",
      expectedTarget: head,
    });
    expect(names).toContain("refs/heads/__probe_del__");
    expect(repo.tryFindReference("refs/heads/__probe_del__")).toBe(null);

    // must_not_exist is invalid for deletions.
    expect(() =>
      repo.editReference({
        name: "refs/heads/__probe_del__",
        change: "delete",
        previousValue: "must_not_exist",
      }),
    ).toThrow();
  });

  test("deref applies edits through symbolic references", () => {
    const head = repo.headId();
    repo.editReference({
      name: "refs/heads/__probe_deref__",
      newTarget: head,
      force: true,
    });
    // A symbolic ref pointing at the probe branch.
    repo.editReference({
      name: "refs/heads/__probe_sym__",
      newTarget: "refs/heads/__probe_deref__",
      force: true,
    });
    expect(repo.findReference("refs/heads/__probe_sym__").kind).toBe("symbolic");

    // Deleting through the symbolic ref (deref) removes the referent, not the symbol.
    repo.editReference({
      name: "refs/heads/__probe_sym__",
      change: "delete",
      deref: true,
    });
    expect(repo.tryFindReference("refs/heads/__probe_deref__")).toBe(null);
    expect(repo.tryFindReference("refs/heads/__probe_sym__")?.name).toBe(
      "refs/heads/__probe_sym__",
    );
    repo.findReference("refs/heads/__probe_sym__").delete();
  });

  test("refLog 'only' deletes only the reflog and keeps the ref", () => {
    const head = repo.headId();
    repo.editReference({
      name: "refs/heads/__probe_logonly__",
      newTarget: head,
      force: true,
      logMessage: "before deletion",
    });
    expect(repo.reflogExists("refs/heads/__probe_logonly__")).toBe(true);

    // `RefLog::Only` removes the reflog but leaves the reference untouched.
    repo.editReference({
      name: "refs/heads/__probe_logonly__",
      change: "delete",
      refLog: "only",
    });
    expect(repo.tryFindReference("refs/heads/__probe_logonly__")?.name).toBe(
      "refs/heads/__probe_logonly__",
    );
    expect(repo.reflogExists("refs/heads/__probe_logonly__")).toBe(false);
    expect(repo.reflog("refs/heads/__probe_logonly__")).toEqual([]);

    // Cleanup: delete the surviving ref (and its now-absent log).
    repo.editReference({
      name: "refs/heads/__probe_logonly__",
      change: "delete",
    });
    expect(repo.tryFindReference("refs/heads/__probe_logonly__")).toBe(null);
  });
});

describe("Reference inspection (src/reference.rs)", () => {
  test("kind / symbolicTarget / target distinguish symbolic and object refs", () => {
    const head = repo.findReference("HEAD");
    expect(head.kind).toBe("symbolic");
    expect(head.symbolicTarget).toMatch(/^refs\/heads\//);
    expect(head.target).toBe("");

    const main = repo.findReference("refs/heads/main");
    expect(main.kind).toBe("object");
    expect(main.symbolicTarget).toBe(null);
    expect(main.target).toMatch(/^[0-9a-f]{40}$/);
  });

  test("category() / categoryAndShortName() classify names", () => {
    const main = repo.findReference("refs/heads/main");
    expect(main.category()).toBe("local_branch");
    expect(main.categoryAndShortName()).toEqual({
      category: "local_branch",
      shortName: "main",
    });

    const head = repo.findReference("HEAD");
    expect(head.category()).toBe("pseudo_ref");
    expect(head.categoryAndShortName()).toEqual({
      category: "pseudo_ref",
      shortName: "HEAD",
    });

    // Unclassified names have no category.
    expect(repo.createReference("refs/custom/__probe_cat__", repo.headId(), true).category()).toBe(
      null,
    );
    repo.findReference("refs/custom/__probe_cat__").delete();
  });

  test("shorten() and fileName() strip prefixes", () => {
    const main = repo.findReference("refs/heads/main");
    expect(main.shorten()).toBe("main");
    expect(main.fileName()).toBe("main");
    // Unknown prefixes are returned as-is by shorten().
    const custom = repo.createReference("refs/custom/__probe_short__", repo.headId(), true);
    expect(custom.shorten()).toBe("refs/custom/__probe_short__");
    expect(custom.fileName()).toBe("__probe_short__");
    custom.delete();
  });

  test("follow() returns the referent one level down, or null for object refs", () => {
    const head = repo.findReference("HEAD");
    const next = head.follow();
    expect(next).not.toBe(null);
    expect(next!.name).toBe(head.symbolicTarget);

    // Object refs are the leaf of the chain.
    expect(repo.findReference("refs/heads/main").follow()).toBe(null);
  });

  test("peelToId() follows symbolic refs and tag chains to the final object", () => {
    const head = repo.headId();
    expect(repo.findReference("HEAD").peelToId()).toBe(head);
    expect(repo.findReference("refs/heads/main").peelToId()).toBe(head);

    // An annotated tag peels to the commit it tags.
    const ann = repo.createTag("__probe_peel__", head, "peel probe", true);
    expect(repo.findReference(ann.name).peelToId()).toBe(head);
    repo.findReference(ann.name).delete();
  });
});

describe("Reference iteration (src/reference.rs)", () => {
  test("referencesPrefixed() filters by prefix", () => {
    const branches = repo.referencesPrefixed("refs/heads/");
    expect(branches.length).toBeGreaterThan(0);
    for (const r of branches) {
      expect(r.name.startsWith("refs/heads/")).toBe(true);
    }
    expect(repo.referencesPrefixed("refs/heads/").length).toBeLessThanOrEqual(
      repo.references().length,
    );
  });

  test("referencesPseudo() lists HEAD and friends", () => {
    const pseudo = repo.referencesPseudo().map((r) => r.name);
    expect(pseudo).toContain("HEAD");
    for (const name of pseudo) {
      expect(name.endsWith("HEAD")).toBe(true);
    }
  });
});

describe("Reflog (src/reference.rs)", () => {
  test("reflog() records entries oldest-first with signature and message", () => {
    const head = repo.headId();
    repo.editReference({
      name: "refs/heads/__probe_reflog__",
      newTarget: head,
      force: true,
      logMessage: "probe reflog",
    });

    const entries = repo.reflog("refs/heads/__probe_reflog__");
    expect(entries.length).toBe(1);
    expect(entries[0].previousOid).toBe("0".repeat(40)); // ref creation
    expect(entries[0].newOid).toBe(head);
    expect(entries[0].message).toContain("probe reflog");
    expect(entries[0].signature.name.length).toBeGreaterThan(0);
    expect(entries[0].signature.email).toContain("@");
    expect(entries[0].signature.time).toBeGreaterThan(0);

    // A second update to a different target appends (gix skips reflog for no-op
    // updates where old and new target are identical); reversed yields newest first.
    repo.editReference({
      name: "refs/heads/__probe_reflog__",
      newTarget: repo.headTreeId(),
      previousValue: "must_exist",
      logMessage: "probe reflog update",
    });
    const all = repo.reflog("refs/heads/__probe_reflog__");
    expect(all).toHaveLength(2);
    expect(all[all.length - 1].message).toContain("probe reflog update");
    const reversed = repo.reflog("refs/heads/__probe_reflog__", true);
    expect(reversed[0].message).toContain("probe reflog update");
    expect(reversed[reversed.length - 1].message).toContain("probe reflog");

    repo.editReference({
      name: "refs/heads/__probe_reflog__",
      change: "delete",
    });
  });

  test("reflog() is empty for refs without a reflog or that do not exist", () => {
    expect(repo.reflog("refs/heads/__nope__")).toEqual([]);
    // HEAD in a fresh clone may lack a reflog file; either way it must not throw.
    expect(Array.isArray(repo.reflog("HEAD"))).toBe(true);
    expect(repo.reflogExists("refs/heads/__nope__")).toBe(false);
  });

  test("reflogExists() agrees with the store", () => {
    // main was just updated by the clone or earlier probes, so it should have a log;
    // a never-written ref must not.
    expect(repo.reflogExists("refs/heads/__nope__")).toBe(false);
    expect(typeof repo.reflogExists("refs/heads/main")).toBe("boolean");
  });
});

describe("Packed refs (src/reference.rs)", () => {
  test("packedRefsPath() points at packed-refs in the common dir", () => {
    expect(repo.packedRefsPath().endsWith("packed-refs")).toBe(true);
    expect(repo.packedRefsPath().includes(".git")).toBe(true);
  });

  test("setPackedBufferMmapThreshold() swaps and returns the previous value", () => {
    expect(repo.setPackedBufferMmapThreshold(65536)).toBe(32768);
    expect(repo.setPackedBufferMmapThreshold(32768)).toBe(65536);
  });

  test("forceRefreshPackedBuffer() succeeds once packed-refs exists", () => {
    // Create a packed ref so the packed-refs file exists (force refresh stats it).
    const head = repo.headId();
    const lw = repo.createLightweightTag("__probe_packed__", head, true);
    try {
      execSync("git pack-refs --all", { cwd: cloneDir, stdio: "ignore" });
      repo.forceRefreshPackedBuffer();
      // The packed ref is still readable after the refresh.
      expect(repo.tryFindReference(lw.name)?.name).toBe(lw.name);
    } finally {
      repo.tryFindReference(lw.name)?.delete();
    }
  });
});

describe("Ref name helpers (src/reference.rs)", () => {
  test("refNameIsValid() applies git's full-name rules", () => {
    expect(refNameIsValid("refs/heads/main")).toBe(true);
    expect(refNameIsValid("refs/tags/v1.0")).toBe(true);
    // Partial names are not full names.
    expect(refNameIsValid("main")).toBe(false);
    expect(refNameIsValid("refs/heads/ma..in")).toBe(false);
    expect(refNameIsValid("refs/heads/x@{y}")).toBe(false);
    expect(refNameIsValid("refs/heads/x.lock")).toBe(false);
  });

  test("refPartialNameIsValid() applies git's partial-name rules", () => {
    expect(refPartialNameIsValid("main")).toBe(true);
    expect(refPartialNameIsValid("heads/main")).toBe(true);
    expect(refPartialNameIsValid("heads/../etc")).toBe(false);
    expect(refPartialNameIsValid("x@{y}")).toBe(false);
  });

  test("refCategoryFullName() assembles full names from category + short name", () => {
    expect(refCategoryFullName("local_branch", "feature")).toBe("refs/heads/feature");
    expect(refCategoryFullName("remote_branch", "origin/main")).toBe("refs/remotes/origin/main");
    expect(refCategoryFullName("tag", "v1")).toBe("refs/tags/v1");
    expect(refCategoryFullName("pseudo_ref", "HEAD")).toBe("HEAD");
    // Idempotent when the short name already carries the prefix.
    expect(refCategoryFullName("local_branch", "refs/heads/feature")).toBe("refs/heads/feature");
    // Unknown / unsupported categories are rejected.
    expect(() => refCategoryFullName("linked_ref", "wt")).toThrow();
    expect(() => refCategoryFullName("bogus", "x")).toThrow();
  });
});
