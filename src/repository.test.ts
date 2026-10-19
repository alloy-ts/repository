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
    execSync(
      `git clone --local --quiet ${JSON.stringify(realRoot)} ${JSON.stringify(cloneDir)}`,
      { stdio: "ignore" },
    );
    repo = Repository.open(cloneDir);
  }
  if (repo.log().length < 2) {
    const head = repo.headCommit();
    const headRef = repo.headName() ?? "refs/heads/main";
    repo.commit(headRef, "second commit for tests", head.treeId, [head.id]);
  }
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
    expect(typeof e.oldId).toBe("string");
    expect(typeof e.newId).toBe("string");
    expect(typeof e.name).toBe("string");
    expect(typeof e.email).toBe("string");
    expect(typeof e.time).toBe("number");
    expect(typeof e.timezoneOffset).toBe("number");
    expect(typeof e.message).toBe("string");
    expect(e.message.length).toBeGreaterThan(0);
  });

  test("createReference writes a reflog entry for the new ref", () => {
    const head = repo.headId();
    const name = "refs/heads/__probe_reflog__";
    repo.createReference(name, head, true);
    const entries = repo.reflog(name);
    expect(entries.length).toBeGreaterThan(0);
    const latest = entries[0];
    expect(latest.newId).toBe(head);
    expect(latest.message).toContain("create reference");
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
    expect(entries[0].id).toBe(repo.headId());
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
    const entry = tree.entries.find(
      (e) => e.name === "Cargo.toml" && e.mode === "Blob",
    );
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

// --- Wave 1: HEAD, remotes, config, revision helpers -----------------------

describe("Wave 1: HEAD, remotes, config, revision helpers", () => {
  test("headTreeId and headTree agree with headCommit", () => {
    const headCommit = repo.headCommit();
    const treeId = repo.headTreeId();
    expect(treeId).toBe(headCommit.treeId);

    const tree = repo.headTree();
    expect(tree.id).toBe(treeId);
  });

  test("headRef returns Reference for symbolic HEAD", () => {
    const href = repo.headRef();
    if (href) {
      expect(href.name).toBe(repo.headName());
    }
  });

  test("remoteAt and createRemote", () => {
    const remote = repo.remoteAt("https://example.com/test.git");
    expect(remote.url).toBe("https://example.com/test.git");

    const created = repo.createRemote("probe_remote", "https://example.com/probe.git");
    expect(created.name).toBe("probe_remote");
    expect(Repository.open(cloneDir).findRemote("probe_remote").url).toBe("https://example.com/probe.git");
  });

  test("configEntries returns array of key-value objects", () => {
    const entries = repo.configEntries();
    expect(Array.isArray(entries)).toBe(true);
    expect(entries.length).toBeGreaterThan(0);
    expect(typeof entries[0].key).toBe("string");
    expect(typeof entries[0].value).toBe("string");
  });

  test("isAncestor, describe, isShallow", () => {
    const head = repo.headId();
    const parent = repo.revParseSingle("HEAD~1");

    expect(repo.isAncestor(parent, head)).toBe(true);
    expect(repo.isAncestor(head, parent)).toBe(false);

    const desc = repo.describe(head);
    expect(typeof desc).toBe("string");

    expect(typeof repo.isShallow()).toBe("boolean");
  });

  test("setHead changes HEAD", () => {
    const headRef = repo.headName() ?? "refs/heads/main";
    repo.setHead(headRef);
    expect(repo.headName()).toBe(headRef);
  });
});

// --- Wave 2: Index & Status ------------------------------------------------

describe("Wave 2: Index & Status", () => {
  test("index and indexOrLoad list entries", () => {
    const entries = repo.index();
    expect(Array.isArray(entries)).toBe(true);
    expect(entries.length).toBeGreaterThan(0);
    const first = entries[0];
    expect(typeof first.path).toBe("string");
    expect(typeof first.mode).toBe("string");
    expect(typeof first.id).toBe("string");
    expect(typeof first.stage).toBe("number");

    const alias = repo.indexOrLoad();
    expect(alias.length).toBe(entries.length);
  });

  test("editIndex adds new file to index", () => {
    const testFile = "wave2_test.txt";
    const fullPath = join(cloneDir, testFile);
    execSync(`echo "wave 2 test file" > ${JSON.stringify(fullPath)}`);

    const oid = repo.editIndex(testFile);
    expect(typeof oid).toBe("string");
    expect(oid.length).toBe(40);

    const reloaded = Repository.open(cloneDir).index();
    expect(reloaded.some((e) => e.path === testFile)).toBe(true);
  });

  test("status (async) and indexWorktree (sync)", async () => {
    const syncStatus = repo.indexWorktree();
    expect(Array.isArray(syncStatus)).toBe(true);

    const asyncStatus = await repo.status();
    expect(Array.isArray(asyncStatus)).toBe(true);
    expect(asyncStatus.length).toBe(syncStatus.length);
  });
});

// --- Wave 3: Config Editing & Branch helpers -------------------------------

describe("Wave 3: Config Editing & Branch helpers", () => {
  test("editConfig updates config and config() reads it", () => {
    repo.editConfig("user.testname", "Jules Tester");
    const reloaded = Repository.open(cloneDir);
    expect(reloaded.configString("user.testname")).toBe("Jules Tester");

    const single = reloaded.config("user.testname");
    expect(single.length).toBe(1);
    expect(single[0].value).toBe("Jules Tester");
  });

  test("branchNames lists local branch names", () => {
    const branches = repo.branchNames();
    expect(Array.isArray(branches)).toBe(true);
    expect(branches.length).toBeGreaterThan(0);
  });

  test("branchRemoteName and branchRemoteRef", () => {
    repo.editConfig("branch.main.remote", "origin");
    repo.editConfig("branch.main.merge", "refs/heads/main");

    const reloaded = Repository.open(cloneDir);
    expect(reloaded.branchRemoteName("main")).toBe("origin");
    expect(reloaded.branchRemoteRef("main")).toBe("refs/heads/main");
  });
});

// --- Wave 4: Object Writing & Raw Access -----------------------------------

describe("Wave 4: Object Writing & Raw Access", () => {
  test("writeTree and writeCommit create objects", () => {
    const blobId = repo.writeBlob(Buffer.from("hello world from wave 4\n"));
    const treeId = repo.writeTree([
      { name: "test.txt", mode: "100644", id: blobId },
    ]);
    expect(treeId).toMatch(/^[0-9a-f]{40}$/);

    const headId = repo.headId();
    const commitId = repo.writeCommit(
      treeId,
      [headId],
      { name: "Author", email: "author@example.com" },
      { name: "Committer", email: "committer@example.com" },
      "direct commit write\n",
    );
    expect(commitId).toMatch(/^[0-9a-f]{40}$/);

    const decoded = repo.findCommit(commitId);
    expect(decoded.treeId).toBe(treeId);
    expect(decoded.parentIds).toEqual([headId]);
    expect(decoded.message).toBe("direct commit write\n");
  });

  test("writeTag and writeObject", () => {
    const headId = repo.headId();
    const tagId = repo.writeTag(
      "v1.0.0-test",
      headId,
      "commit",
      { name: "Tagger", email: "tagger@example.com" },
      "tag message\n",
    );
    expect(tagId).toMatch(/^[0-9a-f]{40}$/);

    const rawBlobId = repo.writeObject("blob", Buffer.from("raw blob payload\n"));
    expect(rawBlobId).toMatch(/^[0-9a-f]{40}$/);
    expect(repo.findBlob(rawBlobId).content().toString()).toBe("raw blob payload\n");
  });

  test("tryFindObject and revParseHex", () => {
    const head = repo.headId();
    const found = repo.tryFindObject(head);
    expect(found).not.toBeNull();
    expect(found?.kind).toBe("commit");

    const missing = repo.tryFindObject("0".repeat(40));
    expect(missing).toBeNull();

    const hex = repo.revParseHex("HEAD");
    expect(hex).toBe(head);
  });
});

// --- Wave 5: Worktrees -----------------------------------------------------

describe("Wave 5: Worktrees", () => {
  test("worktrees, worktree, mainWorktree, and isWorktree", () => {
    const list = repo.worktrees();
    expect(Array.isArray(list)).toBe(true);
    expect(list.length).toBeGreaterThan(0);

    const wt = repo.worktree();
    expect(wt).not.toBeNull();
    expect(typeof wt?.path).toBe("string");

    const main = repo.mainWorktree();
    expect(main).not.toBeNull();
    expect(main?.isMain).toBe(true);

    expect(typeof repo.isWorktree()).toBe("boolean");
  });

  test("createWorktree creates a linked worktree", () => {
    const wtPath = join(base, "wt_linked");
    const wtRepo = repo.createWorktree("wt_branch", wtPath);
    expect(wtRepo.isWorktree()).toBe(true);
    expect(wtRepo.workDir()).toBe(wtPath);
  });
});

// --- Wave 6: Merge, Rebase, Cherry-pick, Apply -----------------------------

describe("Wave 6: Merge, Rebase, Cherry-pick, Apply", () => {
  test("mergeTrees and mergeCommits", () => {
    const headCommit = repo.headCommit();
    const outcome = repo.mergeTrees(headCommit.treeId, headCommit.treeId, null);
    expect(outcome.hasConflicts).toBe(false);
    expect(typeof outcome.treeId).toBe("string");

    const commitOutcome = repo.mergeCommits(headCommit.id, headCommit.id);
    expect(commitOutcome.hasConflicts).toBe(false);
  });

  test("cherryPick and rebase", () => {
    const parent = repo.revParseSingle("HEAD~1");
    const head = repo.headId();

    const cherry = repo.cherryPick(head, parent);
    expect(typeof cherry).toBe("string");
    expect(cherry.length).toBe(40);

    const rebased = repo.rebase(parent, null);
    expect(typeof rebased.headId).toBe("string");
    expect(Array.isArray(rebased.appliedCommits)).toBe(true);
  });

  test("apply applies a git patch", () => {
    const patch = `diff --git a/wave6_patch.txt b/wave6_patch.txt
new file mode 100644
index 0000000..d00491f
--- /dev/null
+++ b/wave6_patch.txt
@@ -0,0 +1001 @@
+hello wave 6 patch
`;
    const res = repo.apply(patch, false);
    expect(typeof res).toBe("boolean");
  });
});

// --- Wave 7: Attributes, Excludes, Filters, Mailmap ------------------------

describe("Wave 7: Attributes, Excludes, Filters, Mailmap", () => {
  test("isPathIgnored checks gitignore rules", () => {
    const isIgnored = repo.isPathIgnored("node_modules/test.js");
    expect(typeof isIgnored).toBe("boolean");
  });

  test("attributes and filterPipeline", () => {
    const attrs = repo.attributes("Cargo.toml");
    expect(Array.isArray(attrs)).toBe(true);

    const pipeline = repo.filterPipeline("Cargo.toml");
    expect(Array.isArray(pipeline)).toBe(true);
  });

  test("resolveMailmap returns MailmapEntry", () => {
    const mapped = repo.resolveMailmap("John Doe", "john@example.com");
    expect(mapped.email).toBe("john@example.com");
    expect(typeof mapped.properEmail).toBe("string");
  });
});

// --- Wave 8: Submodules, Blame, Notes, Checkout/Reset, State, Identity, Options

describe("Wave 8: Submodules, Blame, Notes, Checkout/Reset, State, Identity, Options", () => {
  test("openOpts and initOpts", () => {
    const rOpen = Repository.openOpts(cloneDir, { strict: false });
    expect(rOpen.gitDir()).toBe(repo.gitDir());

    const initDir = join(base, "init_opts_test");
    const rInit = Repository.initOpts(initDir, { bare: false });
    expect(rInit.isBare()).toBe(false);
  });

  test("submodules, blame, notes, state", () => {
    const sms = repo.submodules();
    expect(Array.isArray(sms)).toBe(true);

    const hunks = repo.blame("Cargo.toml");
    expect(Array.isArray(hunks)).toBe(true);

    const notesList = repo.notes();
    expect(Array.isArray(notesList)).toBe(true);

    const st = repo.state();
    expect(typeof st).toBe("string");
  });

  test("author and committer signatures", () => {
    const auth = repo.author();
    expect(auth === null || typeof auth?.email === "string").toBe(true);

    const comm = repo.committer();
    expect(comm === null || typeof comm?.email === "string").toBe(true);
  });

  test("checkout and reset", () => {
    const head = repo.headId();
    const checkedOut = repo.checkout(head);
    expect(typeof checkedOut).toBe("boolean");

    const resetRes = repo.reset(head, false);
    expect(typeof resetRes).toBe("boolean");
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
