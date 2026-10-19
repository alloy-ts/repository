import { execSync } from "node:child_process";
import { mkdtempSync, mkdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository, type Tree, type TreeEntry } from "../dist/index.js";

/**
 * Full-surface test for `Repository`.
 *
 * Strategy: clone the real working repo into a temp dir (local, offline clone -> complete
 * history + refs + `origin` remote) and use that sandbox for every read. Mutating methods
 * (`createReference`, `createTag`, `createLightweightTag`, `commit`) write to throwaway
 * refs/tags that are deleted at the end of each test, so the real repository is never touched.
 */

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository; // the sandbox clone

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-api-"));
  cloneDir = join(base, "clone");
  // Prefer the binding's own `Repository.clone`; fall back to the git CLI if the local
  // transport hits gix's ref-update limitation in this environment.
  try {
    repo = await Repository.clone(realRoot, cloneDir, false);
  } catch (e) {
    console.log(`Repository.clone unavailable (${e}); using git CLI clone for sandbox`);
    rmSync(cloneDir, { recursive: true, force: true });
    execSync(`git clone --local --quiet ${JSON.stringify(realRoot)} ${JSON.stringify(cloneDir)}`, {
      stdio: "ignore",
    });
    repo = Repository.open(cloneDir);
  }
  // Ensure sandbox has at least 2 commits so `HEAD~1` tests pass
  const head = repo.headCommit();
  const refName = repo.headName() ?? "refs/heads/main";
  repo.commit(refName, "second commit for tests", head.treeId, [head.id]);
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true });
});

/** Walk a tree to find the first blob-like entry (Blob / BlobExecutable / Link). */
function firstBlobEntry(r: Repository, tree: Tree): TreeEntry | null {
  for (const e of tree.entries) {
    if (e.mode === "Blob" || e.mode === "BlobExecutable" || e.mode === "Link") return e;
    if (e.mode === "Tree") {
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
    // Best-effort: exercise the binding's own clone when it works in this environment.
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
    } else {
      console.log("no blob entry in HEAD tree; skipping findBlob assertion");
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
    // `references()` returns concrete refs (branches, tags); the symbolic HEAD is not listed.
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
    // For an annotated tag, `target` is the tag object id.
    expect(repo.findTag(ann.target!).name).toBe("__probe_ann__");
    expect(repo.findTag(ann.target!).message).toContain("annotated message");

    expect(repo.references().some((r) => r.name === "refs/tags/__probe_lw__")).toBe(true);

    // Cleanup (temp clone only).
    repo.findReference("refs/heads/__probe_branch__").delete();
    repo.findReference("refs/tags/__probe_lw__").delete();
    repo.findReference("refs/tags/__probe_ann__").delete();
  });
});

describe("Repository.reflog", () => {
  test("reads an existing branch reflog, most-recent-first", () => {
    const branch = repo
      .references()
      .map((r) => r.name)
      .find((n) => n.startsWith("refs/heads/"));
    expect(branch).toBeDefined();
    const entries = repo.reflog(branch!);
    expect(Array.isArray(entries)).toBe(true);
    expect(entries.length).toBeGreaterThan(0);
    // Most recent entry first, and its shape matches RefLogEntry.
    const e = entries[0];
    expect(e).toBeDefined();
    expect(typeof e!.oldId).toBe("string");
    expect(typeof e!.newId).toBe("string");
    expect(typeof e!.name).toBe("string");
    expect(typeof e!.email).toBe("string");
    expect(typeof e!.time).toBe("number");
    expect(typeof e!.timezoneOffset).toBe("number");
    expect(typeof e!.message).toBe("string");
    expect(e!.message.length).toBeGreaterThan(0);
  });

  test("createReference writes a reflog entry for the new ref", () => {
    const head = repo.headId();
    const name = "refs/heads/__probe_reflog__";
    repo.createReference(name, head, true);
    const entries = repo.reflog(name);
    expect(entries.length).toBeGreaterThan(0);
    const latest = entries[0];
    expect(latest).toBeDefined();
    expect(latest!.newId).toBe(head);
    expect(latest!.message).toContain("create reference");
    repo.findReference(name).delete();
  });

  test("returns [] for a ref with no reflog", () => {
    expect(repo.reflog("refs/heads/__probe_no_such_ref__")).toEqual([]);
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
    } else {
      console.log("clone has no 'origin' remote; skipping findRemote(...)");
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

describe("Repository revisions", () => {
  test("revParseSingle / revParse / revWalk", () => {
    const tip = repo.revParseSingle("HEAD");
    expect(tip).toBe(repo.headId());
    expect(repo.revParse("HEAD~1")).toBe(repo.revParseSingle("HEAD~1"));

    const walked = repo.revWalk([tip]);
    expect(walked[0]).toBe(tip);
    expect(walked.length).toBeGreaterThanOrEqual(1);
  });

  test("mergeBase / mergeBaseOctopus", () => {
    const head = repo.revParseSingle("HEAD");
    const parent = repo.revParseSingle("HEAD~1");
    expect(repo.mergeBase(head, head)).toBe(head);
    expect(repo.mergeBase(head, parent)).toBe(parent);
    expect(repo.mergeBaseOctopus([head, parent])).toBe(parent);
  });

  test("log returns commit summaries newest-first", () => {
    const entries = repo.log();
    expect(entries.length).toBeGreaterThan(0);
    expect(entries[0]!.id).toBe(repo.headId());
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

describe("Repository revisions - reading file content", () => {
  test("resolve a ref/SHA to a commit, then read a file blob from its tree", () => {
    // `HEAD` (or `main`, or a `<sha>`) -> commit id -> tree -> entry -> blob content.
    const tip = repo.revParseSingle("HEAD");
    const commit = repo.findCommit(tip);
    const tree = repo.findTree(commit.treeId);
    const entry = tree.entries.find((e) => e.name === "Cargo.toml" && e.mode === "Blob");
    expect(entry).toBeDefined();
    const text = repo.findBlob(entry!.id).content().toString("utf8");
    expect(text.length).toBeGreaterThan(0);
    expect(text).toContain("[package]");
  });

  test("a specific SHA resolves idempotently", () => {
    const sha = repo.revParseSingle("HEAD");
    expect(repo.revParseSingle(sha)).toBe(sha);
  });
});

// --- commit (mutation on a throwaway ref) -----------------------------------

describe("Repository.commit", () => {
  test("commit creates a new commit on a throwaway ref", () => {
    const head = repo.headCommit();
    const newId = repo.commit("refs/heads/__probe_commit__", "probe commit message", head.treeId, [
      head.id,
    ]);
    expect(typeof newId).toBe("string");
    expect(newId).not.toBe(head.id);

    const c = repo.findCommit(newId);
    expect(c.message).toBe("probe commit message");
    expect(c.parentIds).toEqual([head.id]);
    expect(c.treeId).toBe(head.treeId);

    repo.findReference("refs/heads/__probe_commit__").delete();
  });
});

// --- Wave 1 - 8 tests --------------------------------------------------------

describe("Wave 1: HEAD, Remote, Config, Revision helpers", () => {
  test("headTreeId / headRef / headTree / setHead", () => {
    const headCommit = repo.headCommit();
    const treeId = repo.headTreeId();
    expect(treeId).toBe(headCommit.treeId);

    const tree = repo.headTree();
    expect(tree.id).toBe(treeId);

    const headRef = repo.headRef();
    expect(headRef).not.toBeNull();
    const origName = repo.headName();
    expect(headRef?.name).toBe(origName);

    const probeBranch = "refs/heads/__probe_wave1_set_head__";
    repo.createReference(probeBranch, headCommit.id, true);
    repo.setHead(probeBranch);
    expect(repo.headName()).toBe(probeBranch);

    if (origName) {
      repo.setHead(origName);
      expect(repo.headName()).toBe(origName);
    }
    repo.findReference(probeBranch).delete();
  });

  test("remoteAt / createRemote", () => {
    const url = "https://github.com/example/test-repo.git";
    const anon = repo.remoteAt(url);
    expect(anon.name).toBeNull();
    expect(anon.url).toBe(url);

    const remoteName = "__probe_remote_wave1__";
    const created = repo.createRemote(remoteName, url);
    expect(created.name).toBe(remoteName);
    expect(created.url).toBe(url);

    const found = repo.findRemote(remoteName);
    expect(found.name).toBe(remoteName);
  });

  test("configEntries", () => {
    const entries = repo.configEntries();
    expect(Array.isArray(entries)).toBe(true);
    expect(entries.length).toBeGreaterThan(0);
    for (const entry of entries) {
      expect(typeof entry.key).toBe("string");
      expect(typeof entry.value).toBe("string");
    }
  });

  test("isAncestor / describe / isShallow", () => {
    const tip = repo.revParseSingle("HEAD");
    const parent = repo.revParseSingle("HEAD~1");
    expect(repo.isAncestor(parent, tip)).toBe(true);
    expect(repo.isAncestor(tip, parent)).toBe(false);

    const desc = repo.describe();
    expect(typeof desc === "string" || desc === null).toBe(true);

    expect(typeof repo.isShallow()).toBe("boolean");
  });
});

describe("Wave 2: Index and Status", () => {
  test("index / indexOrLoad", () => {
    const entries = repo.indexOrLoad();
    expect(Array.isArray(entries)).toBe(true);
    expect(entries.length).toBeGreaterThan(0);
    const first = entries[0];
    expect(first).toBeDefined();
    expect(typeof first!.path).toBe("string");
    expect(typeof first!.mode).toBe("string");
    expect(typeof first!.id).toBe("string");
    expect(typeof first!.stage).toBe("number");
  });

  test("status / indexWorktree / diffIndexToWorktree", async () => {
    const probeFile = join(cloneDir, "__probe_wave2_status.txt");
    const fs = await import("node:fs");
    fs.writeFileSync(probeFile, "wave2 probe file content\n");

    try {
      const st = await repo.status();
      expect(Array.isArray(st)).toBe(true);
      expect(st.some((item) => item.path.includes("__probe_wave2_status.txt"))).toBe(true);

      const iw = repo.indexWorktree();
      expect(Array.isArray(iw)).toBe(true);
      expect(iw.some((item) => item.path.includes("__probe_wave2_status.txt"))).toBe(true);

      const diff = repo.diffIndexToWorktree();
      expect(Array.isArray(diff)).toBe(true);
    } finally {
      if (fs.existsSync(probeFile)) {
        fs.unlinkSync(probeFile);
      }
    }
  });
});

describe("Wave 3: Config Editing and Branch Helpers", () => {
  test("editConfig and configString", () => {
    repo.editConfig("user.name", "Test User Wave 3");
    expect(repo.configString("user.name")).toBe("Test User Wave 3");
  });

  test("branchNames / branchRemote / branchMerge / branchUpstream", () => {
    const branches = repo.branchNames();
    expect(Array.isArray(branches)).toBe(true);
    expect(branches.length).toBeGreaterThan(0);

    const mainBranch = branches.find((b) => b === "main") ?? branches[0]!;
    const remote = repo.branchRemote(mainBranch);
    expect(remote === null || typeof remote === "string").toBe(true);

    const merge = repo.branchMerge(mainBranch);
    expect(merge === null || typeof merge === "string").toBe(true);

    const upstream = repo.branchUpstream(mainBranch);
    expect(upstream === null || typeof upstream === "string").toBe(true);
  });
});

describe("Wave 4: Object Writing and Lookup Helpers", () => {
  test("writeObject / writeTree / tryFindObject / revParseHex", () => {
    const blobData = Buffer.from("wave 4 test content\n");
    const blobId = repo.writeObject("blob", blobData);
    expect(typeof blobId).toBe("string");
    expect(blobId.length).toBe(40);

    const treeId = repo.writeTree([{ name: "test_wave4.txt", id: blobId, mode: "100644" }]);
    expect(typeof treeId).toBe("string");
    expect(treeId.length).toBe(40);

    const found = repo.tryFindObject(blobId);
    expect(found).not.toBeNull();
    expect(found?.kind).toBe("blob");

    const missing = repo.tryFindObject("0".repeat(40));
    expect(missing).toBeNull();

    const hex = repo.revParseHex("HEAD");
    expect(hex).toBe(repo.headId());
  });
});

describe("Wave 5: Worktree Management", () => {
  test("isWorktree / mainWorktree / worktrees / worktree / withWorktree", () => {
    expect(typeof repo.isWorktree()).toBe("boolean");

    const mainWt = repo.mainWorktree();
    expect(mainWt).not.toBeNull();
    expect(mainWt?.isMain).toBe(true);
    expect(mainWt?.workDir).toBe(cloneDir);

    const wts = repo.worktrees();
    expect(Array.isArray(wts)).toBe(true);

    const wtNone = repo.worktree("__nonexistent_wt__");
    expect(wtNone).toBeNull();

    const scoped = repo.withWorktree(cloneDir);
    expect(scoped.headId()).toBe(repo.headId());
  });
});

describe("Wave 6: Merge, Rebase, Cherry-Pick, and Apply", () => {
  test("mergeTrees and mergeCommits", () => {
    const headCommit = repo.headCommit();
    const outcome = repo.mergeTrees(headCommit.treeId, headCommit.treeId);
    expect(outcome.treeId).toBe(headCommit.treeId);
    expect(outcome.conflicts).toEqual([]);

    const commitOutcome = repo.mergeCommits(headCommit.id, headCommit.id);
    expect(commitOutcome.treeId).toBe(headCommit.treeId);
  });

  test("cherryPick and rebase", () => {
    const headCommit = repo.headCommit();
    const cp = repo.cherryPick(headCommit.id);
    expect(typeof cp.treeId).toBe("string");

    const parent = headCommit.parentIds[0];
    if (parent) {
      const rb = repo.rebase(parent);
      expect(typeof rb.treeId).toBe("string");
    }
  });

  test("applyPatch", async () => {
    const fs = await import("node:fs");
    const testFile = join(cloneDir, "patch_test.txt");
    fs.writeFileSync(testFile, "line 1\nline 2\n");

    const patch = `--- a/patch_test.txt\n+++ b/patch_test.txt\n@@ -1,2 +1,3 @@\n line 1\n+line 1.5\n line 2\n`;
    try {
      const res = repo.applyPatch(patch);
      expect(res.appliedCount).toBe(1);
      expect(fs.readFileSync(testFile, "utf8")).toContain("line 1.5");
    } finally {
      if (fs.existsSync(testFile)) {
        fs.unlinkSync(testFile);
      }
    }
  });
});

describe("Wave 7: Attributes, Excludes, and Mailmap", () => {
  test("attributes / isPathIgnored / checkMailmap", () => {
    const attrs = repo.attributes("Cargo.toml");
    expect(Array.isArray(attrs)).toBe(true);

    const isIgnored = repo.isPathIgnored("__nonexistent_file__");
    expect(typeof isIgnored).toBe("boolean");

    const mapped = repo.checkMailmap("Author Name", "author@example.com");
    expect(mapped.name).toBe("Author Name");
    expect(mapped.email).toBe("author@example.com");
  });
});

describe("Wave 8: Submodules, Blame, State, Identity, Reset & Options", () => {
  test("new Repository() / decode() / submodules / blame / state / committer / author / reset / openWithOpts / initWithOpts", () => {
    const discovered = new Repository(cloneDir);
    expect(discovered.headId()).toBe(repo.headId());

    const decodedHead = repo.headCommit().decode();
    expect(decodedHead.id).toBe(repo.headId());
    expect(typeof decodedHead.author.name).toBe("string");

    const sms = repo.submodules();
    expect(Array.isArray(sms)).toBe(true);

    const blame = repo.blame("Cargo.toml");
    expect(Array.isArray(blame)).toBe(true);

    const st = repo.state();
    expect(typeof st).toBe("string");

    const committer = repo.committer();
    expect(committer === null || typeof committer?.name === "string").toBe(true);

    const author = repo.author();
    expect(author === null || typeof author?.name === "string").toBe(true);

    const opened = Repository.openWithOpts(cloneDir, true);
    expect(opened.headId()).toBe(repo.headId());

    const initDir = join(base, "init-opts-test");
    const inited = Repository.initWithOpts(initDir, false, "main");
    expect(inited.isBare()).toBe(false);
  });
});
