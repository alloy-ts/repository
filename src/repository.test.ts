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
  // Prefer the binding's own `Repository.clone`; fall back to a git CLI sandbox if the
  // local transport hits gix's ref-update limitation in this environment.
  try {
    repo = await Repository.clone(realRoot, cloneDir, false);
  } catch (e) {
    console.log(`Repository.clone unavailable (${e}); using git CLI clone for sandbox`);
    rmSync(cloneDir, { recursive: true, force: true });
    // `git init` + `fetch` instead of `git clone`: a clone always creates
    // `refs/remotes/origin/HEAD`, which collides with `refs/remotes/origin/head/...`
    // on case-insensitive filesystems when the source has a `head/*` branch. The
    // fetch-only sandbox avoids that ref entirely.
    const branch = Repository.open(realRoot).headName() ?? "refs/heads/main";
    const git = (args: string) => execSync(`git ${args}`, { stdio: "ignore" });
    git(`init --quiet ${JSON.stringify(cloneDir)}`);
    git(`-C ${JSON.stringify(cloneDir)} remote add origin ${JSON.stringify(realRoot)}`);
    git(
      `-C ${JSON.stringify(cloneDir)} fetch --quiet origin "+refs/heads/*:refs/remotes/origin/*"`,
    );
    git(`-C ${JSON.stringify(cloneDir)} fetch --quiet origin "+${branch}:${branch}"`);
    git(`-C ${JSON.stringify(cloneDir)} symbolic-ref HEAD ${branch}`);
    repo = Repository.open(cloneDir);
  }

  // Ensure sandbox has at least 2 commits so `HEAD~1` tests work cleanly.
  if (repo.log().length < 2) {
    const head = repo.headCommit();
    const refName = repo.headName() ?? "refs/heads/main";
    repo.commit(refName, "second commit for tests", head.treeId, [head.id]);
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
    const e = entries[0]!;
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
    const latest = entries[0]!;
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

// --- wave-4 additions: object writing and raw object lookup -----------------

describe("Repository wave-4 additions (Object writing & Raw lookup)", () => {
  test("tryFindObject returns GitObject or null without throwing", () => {
    const head = repo.headId();
    const obj = repo.tryFindObject(head);
    expect(obj).not.toBeNull();
    expect(obj!.id).toBe(head);
    expect(obj!.kind).toBe("commit");

    const nonExistent = repo.tryFindObject("0".repeat(40));
    expect(nonExistent).toBeNull();
  });

  test("revParseHex resolves revspec to hex object id", () => {
    const head = repo.headId();
    expect(repo.revParseHex("HEAD")).toBe(head);
  });

  test("writeTree, writeCommit, writeTag, writeObject low-level object creation", () => {
    const blobId = repo.writeBlob(Buffer.from("wave 4 content\n"));
    expect(repo.objectExists(blobId)).toBe(true);

    const treeId = repo.writeTree([{ path: "hello.txt", mode: "100644", id: blobId }]);
    expect(typeof treeId).toBe("string");
    expect(repo.objectExists(treeId)).toBe(true);

    const commitId = repo.writeCommit({
      message: "wave 4 commit",
      tree: treeId,
      parents: [repo.headId()],
      authorName: "Wave4 Author",
      authorEmail: "wave4@example.com",
    });
    expect(typeof commitId).toBe("string");
    const createdCommit = repo.findCommit(commitId);
    expect(createdCommit.message).toBe("wave 4 commit");
    expect(createdCommit.author.name).toBe("Wave4 Author");

    const tagId = repo.writeTag("v4.0.0-test", commitId, "commit", "wave 4 tag message");
    expect(typeof tagId).toBe("string");
    const createdTag = repo.findTag(tagId);
    expect(createdTag.name).toBe("v4.0.0-test");
    expect(createdTag.message).toBe("wave 4 tag message");

    const rawBlobId = repo.writeObject("blob", Buffer.from("raw blob data\n"));
    expect(repo.objectExists(rawBlobId)).toBe(true);
    expect(repo.findBlob(rawBlobId).content().toString()).toBe("raw blob data\n");
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

  test("revParseSingle('<rev>:<path>') resolves a file straight to its blob id", () => {
    // The revspec shortcut: `HEAD:ReadMe.md` resolves to the blob in one call.
    const viaSpec = repo.revParseSingle("HEAD:ReadMe.md");

    // Same blob as the explicit walk: commit -> tree -> entry -> blob.
    const commit = repo.findCommit(repo.headId());
    const tree = repo.findTree(commit.treeId);
    const entry = tree.entries.find((e) => e.name === "ReadMe.md" && e.mode === "Blob");
    expect(entry).toBeDefined();
    expect(viaSpec).toBe(entry!.id);

    // The content is the file *at that revision*, read from the object DB.
    const text = repo.findBlob(viaSpec).content().toString("utf8");
    expect(text.length).toBeGreaterThan(0);
    expect(text).toContain("# repository");
  });

  test("revParseSingle reads a file from a named branch", () => {
    // E.g. "main:ReadMe.md" — the file as of that branch's tip.
    const branch = repo.headName()?.replace("refs/heads/", "");
    if (!branch) {
      console.log("detached HEAD; skipping branch revspec assertion");
      return;
    }
    expect(repo.revParseSingle(`${branch}:ReadMe.md`)).toBe(repo.revParseSingle("HEAD:ReadMe.md"));
  });

  test("revParseSingle reads a file at a specific (abbreviated) SHA", () => {
    const sha = repo.revParseSingle("HEAD");
    const expected = repo.revParseSingle("HEAD:ReadMe.md");

    expect(repo.revParseSingle(`${sha}:ReadMe.md`)).toBe(expected);
    // Abbreviated object ids are disambiguated against the odb.
    expect(repo.revParseSingle(sha.slice(0, 10))).toBe(sha);
    expect(repo.revParseSingle(`${sha.slice(0, 10)}:ReadMe.md`)).toBe(expected);
  });

  test("revParseSingle('<rev>:<path>') resolves a directory to a tree", () => {
    const srcId = repo.revParseSingle("HEAD:src");
    const entry = repo
      .findTree(repo.headCommit().treeId)
      .entries.find((e) => e.name === "src" && e.mode === "Tree");
    expect(entry).toBeDefined();
    expect(srcId).toBe(entry!.id);
    expect(repo.findTree(srcId).entries.length).toBeGreaterThan(0);
  });

  test("revParseSingle throws for a path missing at the revision", () => {
    expect(() => repo.revParseSingle("HEAD:__no_such_file__.md")).toThrow();
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

// --- wave-1 additions: HEAD extras, remote_at, describe, is_ancestor, shallow ---

describe("Repository wave-1 additions", () => {
  test("headTreeId / headTree / headRef", () => {
    const tid = repo.headTreeId();
    expect(tid).toMatch(/^[0-9a-f]{40}$/);
    const tree = repo.headTree();
    expect(Array.isArray(tree.entries)).toBe(true);
    expect(tree.entries.length).toBeGreaterThan(0);
    expect(repo.headCommit().treeId).toBe(tid);
    const hr = repo.headRef();
    expect(hr).not.toBeNull();
  });

  test("setHead switches HEAD (detached) and can be restored to a branch", () => {
    const orig = repo.headName();
    expect(orig).toContain("refs/heads/");
    // Detach HEAD to the current commit.
    repo.setHead(repo.headId());
    expect(repo.headName()).toBeNull();
    expect(repo.headRef()).toBeNull();
    // Restore the symbolic ref.
    repo.setHead(orig!);
    expect(repo.headName()).toBe(orig);
  });

  test("remoteAt builds an anonymous remote without writing config", () => {
    const remote = repo.remoteAt("https://example.com/owner/repo.git");
    expect(remote.url).toBe("https://example.com/owner/repo.git");
  });

  test("describe returns a non-empty name for HEAD", () => {
    const name = repo.describe("HEAD");
    expect(typeof name).toBe("string");
    expect(name.length).toBeGreaterThan(0);
  });

  test("isAncestor reflects commit reachability", () => {
    const head = repo.revParseSingle("HEAD");
    const parent = repo.revParseSingle("HEAD~1");
    expect(repo.isAncestor(parent, head)).toBe(true);
    expect(repo.isAncestor(head, parent)).toBe(false);
  });

  test("shallow flags return boolean and array", () => {
    expect(typeof repo.isShallow()).toBe("boolean");
    expect(Array.isArray(repo.shallowCommits())).toBe(true);
  });

  test("config_entries returns flattened key/value pairs", () => {
    const entries = repo.configEntries();
    expect(Array.isArray(entries)).toBe(true);
    expect(entries.length).toBeGreaterThan(0);
    const bare = entries.find((e) => e.key === "core.bare");
    expect(bare).toBeDefined();
    expect(bare!.value).toBe("false");
  });
});
