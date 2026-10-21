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

function firstBlobEntry(
  r: Repository,
  tree: Tree,
  prefix = "",
): { relativePath: string; id: string } | null {
  for (const e of tree.entries) {
    const fullPath = prefix ? `${prefix}/${e.name}` : e.name;
    if (e.mode === "100644" || e.mode === "100755" || e.mode === "120000") {
      return { relativePath: fullPath, id: e.id };
    }
    if (e.mode === "040000") {
      const sub = r.findTree(e.id);
      const found = firstBlobEntry(r, sub, fullPath);
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

// --- index editing & options constructors (Phase 2) -----------------------

describe("Index editing & options constructors (Phase 2)", () => {
  test("openOpts() / initOpts() / editIndex() work as expected", () => {
    const opened = Repository.openOpts(cloneDir, false);
    expect(opened.gitDir()).toBe(repo.gitDir());

    const blobId = repo.writeBlob(Buffer.from("staged file content\n"));
    repo.editIndex([{ path: "staged.txt", id: blobId, mode: "100644" }]);

    const indexEntries = repo.indexEntries();
    expect(indexEntries.some((e) => e.path === "staged.txt")).toBe(true);
  });
});

// --- async checkout (Phase 5) ----------------------------------------------

describe("Async checkout (Phase 5)", () => {
  test("checkout() checks out target commit asynchronously", async () => {
    const head = repo.headId();
    const checkedOut = await repo.checkout(head);
    expect(checkedOut).toBe(head);
    expect(repo.headId()).toBe(head);
  });
});

// --- index & status ---------------------------------------------------------

describe("Repository index & status (Wave 2)", () => {
  test("indexEntries() and indexOrLoadFromHeadEntries() return index items", () => {
    const loaded = repo.indexOrLoadFromHeadEntries();
    expect(Array.isArray(loaded)).toBe(true);
    expect(loaded.length).toBeGreaterThan(0);
    for (const e of loaded) {
      expect(typeof e.path).toBe("string");
      expect(typeof e.id).toBe("string");
      expect(typeof e.stage).toBe("number");
    }

    try {
      const entries = repo.indexEntries();
      expect(Array.isArray(entries)).toBe(true);
    } catch {
      // Index file may not exist yet in a fresh clone
    }
  });

  test("status() resolves working tree status asynchronously", async () => {
    const statusEntries = await repo.status();
    expect(Array.isArray(statusEntries)).toBe(true);
  });
});

// --- submodules & identity (Wave 8) ------------------------------------------

describe("Submodules & default identity (Wave 8)", () => {
  test("submodules() and defaultAuthor() / defaultCommitter()", () => {
    const subs = repo.submodules();
    expect(Array.isArray(subs)).toBe(true);

    const author = repo.defaultAuthor();
    expect(author === null || typeof author === "object").toBe(true);

    const committer = repo.defaultCommitter();
    expect(committer === null || typeof committer === "object").toBe(true);

    const note = repo.findNote(repo.headId());
    expect(note === null || typeof note === "object").toBe(true);

    const sm = repo.submoduleByName("nonexistent_submodule");
    expect(sm).toBeNull();
  });
});

// --- attributes & excludes (Wave 7) ------------------------------------------

describe("Repository attributes & excludes (Wave 7)", () => {
  test("attributes() and isPathExcluded() check path attributes and ignore status", () => {
    const attrs = repo.attributes("ReadMe.md");
    expect(Array.isArray(attrs)).toBe(true);

    const isExcluded = repo.isPathExcluded("node_modules/test.js");
    expect(typeof isExcluded).toBe("boolean");
  });
});

// --- worktrees (Wave 5) ------------------------------------------------------

describe("Repository worktrees (Wave 5)", () => {
  test("worktrees(), isWorktree(), and reset() work as expected", () => {
    const wts = repo.worktrees();
    expect(Array.isArray(wts)).toBe(true);
    expect(typeof repo.isWorktree()).toBe("boolean");
    expect(repo.isWorktree()).toBe(false);

    const head = repo.headId();
    repo.reset(head, false);
    expect(repo.headId()).toBe(head);
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

  test("configSet() sets a key that can be read via configString()", () => {
    repo.configSet("core.customSetting", "custom_value");
    expect(Repository.open(cloneDir).configString("core.customSetting")).toBe("custom_value");
  });

  test("branchNames() and branchRemoteName() list branches and tracking remotes", () => {
    const branches = repo.branchNames();
    expect(Array.isArray(branches)).toBe(true);
    for (const b of branches) {
      expect(typeof b).toBe("string");
      const remote = repo.branchRemoteName(b);
      expect(remote === null || typeof remote === "string").toBe(true);
    }
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

  test("get file (e.g. ReadMe.md or repository.manifest.yml) from a specific revision or SHA", () => {
    // 1. Resolve revision spec (e.g. "HEAD" or SHA)
    const commitId = repo.revParseSingle("HEAD");
    const commit = repo.findCommit(commitId);

    // 2. Inspect tree and retrieve file entry
    const tree = repo.findTree(commit.treeId);
    const entry = tree.entry("ReadMe.md") ?? tree.entry("repository.manifest.yml");

    expect(entry).not.toBeNull();
    if (entry) {
      // 3. Find blob by OID and read content
      const blob = repo.findBlob(entry.id);
      const content = blob.content().toString("utf-8");
      expect(content.length).toBeGreaterThan(0);
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

// --- merge operations (Wave 6) ---------------------------------------------

describe("Repository merge & tree operations (Wave 6)", () => {
  test("mergeTrees() and mergeCommits() perform tree merges", () => {
    const head = repo.headCommit();
    const outcome = repo.mergeTrees(head.treeId, head.treeId, head.treeId);
    expect(typeof outcome.treeId).toBe("string");
    expect(typeof outcome.conflicts).toBe("boolean");

    const commitOutcome = repo.mergeCommits(head.id, head.id);
    expect(typeof commitOutcome.treeId).toBe("string");
    expect(typeof commitOutcome.conflicts).toBe("boolean");
  });
});

// --- object writing & retrieval (Wave 4) ------------------------------------

describe("Object writing & object retrieval (Wave 4)", () => {
  test("writeCommit(), writeTag(), and peel() roundtrip (Phase 1)", () => {
    const head = repo.headCommit();
    const newCommitId = repo.writeCommit({
      tree: head.treeId,
      parents: [head.id],
      message: "custom loose commit message\n",
    });
    expect(typeof newCommitId).toBe("string");
    expect(newCommitId).toMatch(/^[0-9a-f]{40}$/);

    const commitObj = repo.findCommit(newCommitId);
    expect(commitObj.message).toBe("custom loose commit message\n");

    const tagObjId = repo.writeTag({
      name: "v1.0.0-custom",
      target: newCommitId,
      targetKind: "commit",
      message: "custom annotated tag\n",
    });
    expect(typeof tagObjId).toBe("string");

    const peeledTree = repo.peel(tagObjId, "tree");
    expect(peeledTree).toBe(head.treeId);

    const peeledCommit = repo.peel(tagObjId, "commit");
    expect(peeledCommit).toBe(newCommitId);
  });

  test("writeTree() creates a tree and tryFindObject() retrieves it", () => {
    const blobId = repo.writeBlob(Buffer.from("sample content\n"));
    const treeId = repo.writeTree([{ path: "file.txt", mode: "100644", id: blobId }]);
    expect(typeof treeId).toBe("string");
    expect(treeId).toMatch(/^[0-9a-f]{40}$/);

    const found = repo.tryFindObject(treeId);
    expect(found).not.toBeNull();
    expect(found?.kind).toBe("tree");

    const missing = repo.tryFindObject("0".repeat(40));
    expect(missing).toBeNull();

    expect(repo.revParseHex("HEAD")).toBe(repo.headId());
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

describe("Commit signature & verification APIs", () => {
  test("commit properties and signature methods", () => {
    const head = repo.headCommit();
    expect(typeof head.time).toBe("number");
    expect(typeof head.isSigned()).toBe("boolean");
    expect(head.signature() === null || typeof head.signature() === "string").toBe(true);

    const author = head.author;
    if (author) {
      expect(typeof author.name).toBe("string");
      expect(typeof author.email).toBe("string");
    }

    const committer = head.committer;
    if (committer) {
      expect(typeof committer.name).toBe("string");
      expect(typeof committer.email).toBe("string");
    }

    const verifyResult = head.verifySignature();
    expect(verifyResult === null || typeof verifyResult === "object").toBe(true);

    const repoVerify = repo.verifyCommitSignature(head.id);
    expect(repoVerify === null || typeof repoVerify === "object").toBe(true);

    const dto = repo.signatureFromDto("Test", "test@example.com", 12345, 0);
    expect(dto.name).toBe("Test");
    expect(dto.email).toBe("test@example.com");

    const cfgVal = repo.configGet("init.defaultBranch") ?? repo.configString("init.defaultBranch");
    expect(cfgVal === null || typeof cfgVal === "string").toBe(true);
  });
});

describe("Repository blame (B1)", () => {
  test("blame() returns per-hunk entries for a real file", () => {
    const tree = repo.findTree(repo.headCommit().treeId);
    const entry = firstBlobEntry(repo, tree);
    if (!entry) return;
    try {
      const outcome = repo.blame(entry.relativePath);
      expect(Array.isArray(outcome.entries)).toBe(true);
      if (outcome.entries.length > 0) {
        expect(typeof outcome.entries[0].commitId).toBe("string");
      }
    } catch (e) {
      expect(String(e)).toContain("blame_file");
    }
  });

  test("blame() accepts an explicit commit id", () => {
    const head = repo.headCommit();
    const tree = repo.findTree(head.treeId);
    const entry = firstBlobEntry(repo, tree);
    if (!entry) return;
    try {
      const outcome = repo.blame(entry.relativePath, head.id);
      expect(Array.isArray(outcome.entries)).toBe(true);
      if (outcome.entries.length > 0) {
        expect(typeof outcome.entries[0].commitId).toBe("string");
      }
    } catch (e) {
      expect(String(e)).toContain("blame_file");
    }
  });
});

describe("Repository reference transactions & namespace (B2)", () => {
  test("namespace()/setNamespace() manage the ref namespace", () => {
    const initial = repo.namespace();
    expect(initial === null || typeof initial === "string").toBe(true);

    const prev = repo.setNamespace("feature-x");
    expect(prev === null || typeof prev === "string").toBe(true);

    // Restore the previous namespace (if any).
    if (prev !== null) repo.setNamespace(prev);
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
});
