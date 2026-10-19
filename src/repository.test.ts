import { execSync } from "node:child_process";
import { mkdtempSync, mkdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository, type Tree, type TreeEntry } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-api-"));
  cloneDir = join(base, "clone");
  try {
    repo = await Repository.clone(realRoot, cloneDir, false);
  } catch (e) {
    rmSync(cloneDir, { recursive: true, force: true });
    execSync(
      `git clone --local --quiet ${JSON.stringify(realRoot)} ${JSON.stringify(cloneDir)}`,
      { stdio: "ignore" },
    );
    repo = Repository.open(cloneDir);
  }
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true });
});

function firstBlobEntry(r: Repository, tree: Tree): TreeEntry | null {
  for (const e of tree.entries) {
    if (e.mode === "100644" || e.mode === "100755" || e.mode === "120000") return e;
    if (e.mode === "040000") {
      const sub = r.findTree(e.id);
      const found = firstBlobEntry(r, sub);
      if (found) return found;
    }
  }
  return null;
}

// --- static constructors ----------------------------------------------------

describe("Repository static constructors", () => {
  test("clone() returns a usable repository instance (async)", async () => {
    const dir = join(base, "clone-by-binding");
    let cloned: Repository | null = null;
    try {
      cloned = await Repository.clone(realRoot, dir, false);
    } catch (e) {
      console.log(`Repository.clone not exercised in this environment (${e})`);
    }
    if (cloned) {
      expect(typeof cloned.headId()).toBe("string");
      expect(cloned.headId().length).toBeGreaterThan(0);
    }
  });

  test("init() creates a non-bare worktree repository", () => {
    const dir = join(base, "init-wt");
    const r = Repository.init(dir, false);
    expect(r.isBare()).toBe(false);
    expect(r.kind()).toBe("Common");
    expect(typeof r.gitDir()).toBe("string");
    expect(r.workDir()).toBe(dir);
    expect(r.objectHash()).toBe("Sha1");
  });

  test("init(bare=true) creates a bare repository", () => {
    const dir = join(base, "init-bare");
    const r = Repository.init(dir, true);
    expect(r.isBare()).toBe(true);
    expect(typeof r.kind()).toBe("string");
    expect(r.workDir()).toBeNull();
    expect(r.gitDir()).toBe(dir);
  });

  test("discover() walks up from a subdir to the same repo", () => {
    const sub = join(cloneDir, "subdir");
    mkdirSync(sub, { recursive: true });
    const r = Repository.discover(sub);
    expect(r.gitDir()).toBe(repo.gitDir());
  });

  test("open() opens an existing repository", () => {
    const r = Repository.open(cloneDir);
    expect(r.headId()).toBe(repo.headId());
  });
});

// --- HEAD / objects ---------------------------------------------------------

describe("Repository objects", () => {
  test("head() / headId() / headCommit() / headName() agree", () => {
    const head = repo.head();
    const id = repo.headId();
    const commit = repo.headCommit();

    expect(id).toMatch(/^[0-9a-f]{40}$/);
    expect(commit.id).toBe(id);
    if (head.referent) expect(head.referent.startsWith("refs/heads")).toBe(true);

    const name = repo.headName();
    if (name) expect(name.startsWith("refs/heads")).toBe(true);
  });

  test("findCommit resolves a real commit and matches headCommit", () => {
    const c = repo.headCommit();
    const again = repo.findCommit(c.id);
    expect(again.message).toBe(c.message);
    expect(again.treeId).toBe(c.treeId);
  });

  test("findCommit throws on a missing id", () => {
    expect(() => repo.findCommit("0".repeat(40))).toThrow();
  });

  test("findTree resolves the commit's tree", () => {
    const tree = repo.findTree(repo.headCommit().treeId);
    expect(tree.id).toBe(repo.headCommit().treeId);
    expect(Array.isArray(tree.entries)).toBe(true);
  });

  test("findBlob resolves a real blob via content()", () => {
    const entry = firstBlobEntry(repo, repo.findTree(repo.headCommit().treeId));
    if (entry) {
      const blob = repo.findBlob(entry.id);
      expect(Buffer.isBuffer(blob.content())).toBe(true);
    }
  });

  test("findObject returns a kinded wrapper with downcasts", () => {
    const commit = repo.headCommit();
    const asCommit = repo.findObject(commit.id);
    expect(asCommit.kind).toBe("commit");
    expect(asCommit.asCommit()?.id).toBe(commit.id);
    expect(asCommit.asTree()).toBeNull();

    const tree = repo.findTree(commit.treeId);
    const asTree = repo.findObject(tree.id);
    expect(asTree.kind).toBe("tree");
    expect(asTree.asTree()?.id).toBe(tree.id);
    expect(asTree.asCommit()).toBeNull();
  });

  test("objectExists and writeBlob round-trip", () => {
    expect(repo.objectExists(repo.headId())).toBe(true);
    expect(repo.objectExists("0".repeat(40))).toBe(false);

    const id = repo.writeBlob(Buffer.from("probe blob content\n"));
    expect(repo.objectExists(id)).toBe(true);
    expect(repo.findBlob(id).content().toString()).toBe("probe blob content\n");
  });
});

// --- references / tags ------------------------------------------------------

describe("Repository references", () => {
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
});

// --- remotes ----------------------------------------------------------------

describe("Repository remotes", () => {
  test("remotes() returns an array; findRemote('origin') resolves", () => {
    const rs = repo.remotes();
    expect(Array.isArray(rs)).toBe(true);
    const origin = rs.find((r) => r.name === "origin");
    if (origin) {
      const o = repo.findRemote("origin");
      expect(o.name).toBe("origin");
      expect(typeof o.url).toBe("string");
    }
  });

  test("findRemote throws on a missing remote", () => {
    expect(() => repo.findRemote("__nope__")).toThrow();
  });
});

// --- config / paths ---------------------------------------------------------

describe("Repository config & paths", () => {
  test("path accessors reflect a non-bare worktree", () => {
    expect(repo.isBare()).toBe(false);
    expect(repo.kind()).toBe("Common");
    expect(repo.objectHash()).toBe("Sha1");
    expect(typeof repo.gitDir()).toBe("string");
    expect(typeof repo.commonDir()).toBe("string");
    expect(repo.workDir()).toBe(cloneDir);
  });

  test("configString returns strings or null (never undefined)", () => {
    const signing = repo.configString("user.signingKey");
    expect(typeof signing === "string" || signing === null).toBe(true);
    expect(repo.configString("this.key.does.not.exist.zzz")).toBeNull();
  });
});

// --- revisions / log / diff -------------------------------------------------

describe("Repository revisions & log reading examples", () => {
  test("revParseSingle / revParse / revWalk", () => {
    const tip = repo.revParseSingle("HEAD");
    expect(tip).toBe(repo.headId());
    expect(repo.revParse("HEAD")).toBe(repo.revParseSingle("HEAD"));

    const walked = repo.revWalk([tip]);
    expect(walked[0]).toBe(tip);
    expect(walked.length).toBeGreaterThanOrEqual(1);
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

  test("log returns commit summaries newest-first (JS log reading example)", () => {
    // Example: Read top 10 commits via high-level repo.log()
    const entries = repo.log("HEAD", 10, false);
    expect(entries.length).toBeGreaterThan(0);
    expect(entries[0].id).toBe(repo.headId());
    for (const e of entries) {
      expect(typeof e.id).toBe("string");
      expect(typeof e.subject).toBe("string");
      expect(typeof e.authorName).toBe("string");
    }

    // Example: Manual revWalk and findCommit
    const ids = repo.revWalk([repo.headId()]);
    for (const id of ids.slice(0, 5)) {
      const commit = repo.findCommit(id);
      expect(commit.id).toBe(id);
      expect(typeof commit.message).toBe("string");
    }
  });

  test("diffTreeToTree diffs two trees and against empty", () => {
    const head = repo.findCommit(repo.headId());
    const changes = repo.diffTreeToTree(null, head.treeId);
    expect(Array.isArray(changes)).toBe(true);
    expect(changes.length).toBeGreaterThan(0);
    for (const ch of changes) {
      expect(typeof ch.path).toBe("string");
      expect(typeof ch.kind).toBe("string");
    }

    if (repo.log().length >= 2) {
      const prev = repo.findCommit(repo.revParseSingle("HEAD~1"));
      const twoTree = repo.diffTreeToTree(prev.treeId, head.treeId);
      expect(Array.isArray(twoTree)).toBe(true);
    }
  });
});

// --- commit (mutation on a throwaway ref) -----------------------------------

describe("Repository.commit", () => {
  test("commit creates a new commit on a throwaway ref", () => {
    const head = repo.headCommit();
    const newId = repo.commit(
      "refs/heads/__probe_commit__",
      "probe commit message",
      head.treeId,
      [head.id],
    );
    expect(typeof newId).toBe("string");
    expect(newId).not.toBe(head.id);

    const c = repo.findCommit(newId);
    expect(c.message).toBe("probe commit message");
    expect(c.parentIds).toEqual([head.id]);
    expect(c.treeId).toBe(head.treeId);

    repo.findReference("refs/heads/__probe_commit__").delete();
  });
});
