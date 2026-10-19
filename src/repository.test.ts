import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { expect, test } from "vite-plus/test";
import { Repository, type LogEntry, type Tree, type TreeEntry } from "./main.ts";

/**
 * JS Examples & Tests for Repository:
 *
 * 1. Static Constructors:
 *    - Repository.open(path)        -> open repository at explicit path
 *    - Repository.discover(path?)   -> walk parent directories to find .git
 *    - Repository.init(path, bare?) -> initialize a new repository
 *    - Repository.clone(url, path)  -> async clone a repository
 *
 * 2. Reading Git Logs:
 *    - repo.log(rev?, limit?, firstParent?) -> obtain log summaries in reverse-chronological order
 *    - repo.revWalk(tips) + repo.findCommit(id) -> manual revision walking and object inspection
 *
 * 3. Reading Reflogs:
 *    - repo.reflog("refs/heads/main") -> read reflog entries for a branch
 *    - repo.headReflog()              -> read reflog entries for HEAD
 *
 * 4. Manifest Remote Setup:
 *    - repo.setupRemotesFromManifest(manifestPath?) -> setup remotes from repository.manifest.yml
 *
 * 5. Revisions & Entry Reading:
 *    - repo.revParseSingle("HEAD:ReadMe.md") -> get blob ID for path at revision
 *    - repo.findBlob(blobId).content()       -> read raw buffer/text content at revision
 *
 * 6. Worktree Inspection:
 *    - repo.worktrees()    -> list worktree information
 *    - repo.isWorktree()   -> check if repo has working directory
 *    - repo.mainWorktree() -> get main worktree info
 *
 * 7. Mailmap Resolution:
 *    - repo.mailmapResolve(name, email) -> resolve author/committer via .mailmap
 *
 * 8. Submodules & State:
 *    - repo.submodules() -> list submodules
 *    - repo.state()      -> get repo operation state
 *    - repo.committer()  -> get default committer
 *    - repo.emptyTreeId() -> get empty tree ID
 *
 * 9. Inspection of Trees and Objects.
 */

function openRepo(): Repository {
  return Repository.open(process.cwd());
}

/** Render a tree as an `ls -1` style listing, with the mode and id prefix. */
function renderTree(tree: Tree): string {
  return tree.entries
    .map((e: TreeEntry) => `${e.mode.padEnd(7)} ${e.id.slice(0, 8)} ${e.name}`)
    .join("\n");
}

// --- 1. Static constructors ---------------------------------------------------

test("Repository static constructor: open", () => {
  const repo = Repository.open(process.cwd());
  expect(repo).toBeInstanceOf(Repository);
  expect(typeof repo.gitDir()).toBe("string");
  expect(repo.isBare()).toBe(false);
});

test("Repository static constructor: discover", () => {
  const repo = Repository.discover();
  expect(repo).toBeInstanceOf(Repository);
  expect(typeof repo.gitDir()).toBe("string");
});

test("Repository static constructor: init", () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "repo-test-init-"));
  try {
    const repo = Repository.init(tmpDir, false);
    expect(repo).toBeInstanceOf(Repository);
    expect(repo.isBare()).toBe(false);

    const bareTmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "repo-test-init-bare-"));
    try {
      const bareRepo = Repository.init(bareTmpDir, true);
      expect(bareRepo).toBeInstanceOf(Repository);
      expect(bareRepo.isBare()).toBe(true);
    } finally {
      fs.rmSync(bareTmpDir, { recursive: true, force: true });
    }
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test("Repository static constructor: clone", async () => {
  const targetDir = fs.mkdtempSync(path.join(os.tmpdir(), "repo-test-clone-"));
  try {
    // Clone local repository directory
    const repo = await Repository.clone(process.cwd(), targetDir, false);
    expect(repo).toBeInstanceOf(Repository);
    expect(typeof repo.gitDir()).toBe("string");
    expect(repo.isBare()).toBe(false);
  } finally {
    fs.rmSync(targetDir, { recursive: true, force: true });
  }
});

// --- 2. Reading Git Logs ------------------------------------------------------

test("repo.log() reads history from HEAD", () => {
  const repo = openRepo();
  const logs: LogEntry[] = repo.log();

  expect(Array.isArray(logs)).toBe(true);
  expect(logs.length).toBeGreaterThan(0);

  const headEntry = logs[0]!;
  expect(typeof headEntry.id).toBe("string");
  expect(headEntry.id.length).toBe(40);
  expect(typeof headEntry.subject).toBe("string");
  expect(typeof headEntry.authorName).toBe("string");
  expect(typeof headEntry.authorEmail).toBe("string");
  expect(typeof headEntry.authorTime).toBe("number");
  expect(Array.isArray(headEntry.parentIds)).toBe(true);

  // Example JS logging format:
  const formatted = logs.slice(0, 5).map((e) => {
    const date = new Date(e.authorTime * 1000).toISOString().slice(0, 10);
    return `${e.id.slice(0, 8)} ${date} ${e.authorName}: ${e.subject}`;
  });
  console.log("\nRecent commit log:\n" + formatted.join("\n"));
});

test("repo.log() accepts limit parameter", () => {
  const repo = openRepo();
  const limit = 2;
  const logs = repo.log(undefined, limit);

  expect(logs.length).toBeLessThanOrEqual(limit);
  expect(logs.length).toBeGreaterThan(0);
});

test("repo.log() accepts firstParent flag", () => {
  const repo = openRepo();
  const allLogs = repo.log(undefined, 10, false);
  const firstParentLogs = repo.log(undefined, 10, true);

  expect(Array.isArray(firstParentLogs)).toBe(true);
  expect(firstParentLogs.length).toBeGreaterThan(0);
  expect(firstParentLogs[0]!.id).toBe(allLogs[0]!.id);
});

test("manual log reading via revWalk() and findCommit()", () => {
  const repo = openRepo();
  const headId = repo.headId();
  const ids = repo.revWalk([headId]);

  expect(Array.isArray(ids)).toBe(true);
  expect(ids.length).toBeGreaterThan(0);
  expect(ids[0]).toBe(headId);

  const commit = repo.findCommit(ids[0]!);
  expect(commit.id).toBe(headId);
  expect(typeof commit.message).toBe("string");
  expect(typeof commit.author.name).toBe("string");
  expect(typeof commit.author.email).toBe("string");
});

// --- 3. Repository Reflogs ----------------------------------------------------

test("repo.reflog() reads reflog entries from a reference or HEAD", () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "repo-test-repo-reflog-"));
  try {
    const repo = Repository.init(tmpDir, false);
    const treeId = Repository.open(process.cwd()).headCommit().treeId;

    const commit1 = repo.commit("refs/heads/main", "initial commit", treeId, []);
    const commit2 = repo.commit("refs/heads/main", "second commit", treeId, [commit1]);

    // Read reflog for refs/heads/main directly via repo
    const logs = repo.reflog("refs/heads/main");
    expect(Array.isArray(logs)).toBe(true);
    expect(logs.length).toBeGreaterThanOrEqual(1);
    expect(logs[0]!.newId).toBe(commit2);
    expect(logs[0]!.previousId).toBe(commit1);

    // Read HEAD reflog via repo.headReflog()
    const headLogs = repo.headReflog();
    expect(Array.isArray(headLogs)).toBe(true);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

// --- 4. Manifest Remote Setup -------------------------------------------------

test("setupRemotesFromManifest parses manifest and configures remotes", () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "repo-test-manifest-remotes-"));
  try {
    const repo = Repository.init(tmpDir, false);
    const manifestPath = path.join(process.cwd(), "repository.manifest.yml");
    const configured = repo.setupRemotesFromManifest(manifestPath);

    expect(Array.isArray(configured)).toBe(true);
    expect(configured.length).toBeGreaterThan(0);

    const names = configured.map((r) => r.name);
    expect(names).toContain("origin");
    expect(names).toContain("codeberg");
    expect(names).toContain("github");
    expect(names).toContain("gitlab");

    const reloaded = Repository.open(tmpDir);
    const githubRemote = reloaded.findRemote("github");
    expect(githubRemote).toBeDefined();
    expect(githubRemote.url).toBe("https://github.com/yumin-chen/repository");
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

// --- 5. Revisions & Entry Reading ---------------------------------------------

test("resolve revision for a path and read file content (e.g. ReadMe.md from main/HEAD)", () => {
  const repo = openRepo();

  // 1. Get blob ID for ReadMe.md directly from HEAD/main spec
  const blobIdFromSpec = repo.revParseSingle("HEAD:ReadMe.md");
  expect(typeof blobIdFromSpec).toBe("string");
  expect(blobIdFromSpec.length).toBe(40);

  // 2. Read the file content from the blob
  const blob = repo.findBlob(blobIdFromSpec);
  const content = blob.content().toString("utf-8");
  expect(content).toContain("repository");

  // 3. Resolve commit by branch name, tree entry, and read blob
  const headCommit = repo.headCommit();
  const tree = headCommit.tree();
  const readmeEntry = tree.entry("ReadMe.md");
  expect(readmeEntry).not.toBeNull();
  expect(readmeEntry!.id).toBe(blobIdFromSpec);

  // 4. Resolve file content from specific SHA revision spec
  const shaSpec = `${headCommit.id}:ReadMe.md`;
  const blobIdFromSha = repo.revParseSingle(shaSpec);
  expect(blobIdFromSha).toBe(blobIdFromSpec);
});

// --- Wave 4: Object Writing & Raw Finding -------------------------------------

test("Wave 4: writeTree, tryFindObject, tryFindHeader, revParseHex", () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "repo-test-wave4-"));
  try {
    const repo = Repository.init(tmpDir, false);
    const blob1 = repo.writeBlob(Buffer.from("file one content"));
    const blob2 = repo.writeBlob(Buffer.from("file two content"));

    // writeTree
    const treeId = repo.writeTree([
      { name: "file1.txt", id: blob1, mode: "100644" },
      { name: "file2.sh", id: blob2, mode: "100755" },
    ]);
    expect(typeof treeId).toBe("string");
    expect(treeId.length).toBe(40);

    // tryFindObject
    const obj = repo.tryFindObject(treeId);
    expect(obj).not.toBeNull();
    expect(obj!.kind).toBe("tree");

    const nonExistent = repo.tryFindObject("0000000000000000000000000000000000000000");
    expect(nonExistent).toBeNull();

    // tryFindHeader
    const header = repo.tryFindHeader(treeId);
    expect(header).not.toBeNull();
    expect(header!.kind).toBe("tree");
    expect(typeof header!.size).toBe("number");

    // revParseHex
    const commitId = repo.commit("refs/heads/main", "test commit", treeId, []);
    const parsedHex = repo.revParseHex("refs/heads/main");
    expect(parsedHex).toBe(commitId);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

// --- Wave 3: Config Editing & Branch Helpers --------------------------------

test("Wave 3: setConfigValue, branchNames, branchRemoteName, deleteLocalBranches", () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "repo-test-wave3-"));
  try {
    const repo = Repository.init(tmpDir, false);
    const headCommitTree = Repository.open(process.cwd()).headCommit().treeId;

    const commit1 = repo.commit("refs/heads/main", "first commit", headCommitTree, []);
    repo.createReference("refs/heads/feature", commit1);

    // setConfigValue & configString
    repo.setConfigValue("custom.setting", "Test Setting");
    expect(repo.configString("custom.setting")).toBe("Test Setting");

    repo.setConfigValue("branch.main.remote", "origin");
    expect(repo.branchRemoteName("main")).toBe("origin");

    const branches = repo.branchNames();
    expect(Array.isArray(branches)).toBe(true);

    // deleteLocalBranches
    const deleted = repo.deleteLocalBranches(["refs/heads/feature"]);
    expect(deleted).toContain("refs/heads/feature");
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

// --- Wave 5: Worktrees --------------------------------------------------------

test("Wave 5: worktrees, isWorktree, mainWorktree", () => {
  const repo = openRepo();
  expect(repo.isWorktree()).toBe(true);

  const worktrees = repo.worktrees();
  expect(Array.isArray(worktrees)).toBe(true);
  expect(worktrees.length).toBeGreaterThan(0);

  const mainWt = repo.mainWorktree();
  expect(mainWt).not.toBeNull();
  expect(mainWt!.isMain).toBe(true);
  expect(typeof mainWt!.gitDir).toBe("string");
  expect(typeof mainWt!.workDir).toBe("string");
});

// --- Wave 6: Merge & Rebase Helpers ------------------------------------------

test("Wave 6: mergeBase, mergeBaseOctopus, isAncestor", () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "repo-test-wave6-"));
  try {
    const repo = Repository.init(tmpDir, false);
    const treeId = Repository.open(process.cwd()).headCommit().treeId;

    const root = repo.commit("refs/heads/main", "root commit", treeId, []);
    const b1 = repo.commit("refs/heads/branch1", "branch 1 commit", treeId, [root]);
    const b2 = repo.commit("refs/heads/branch2", "branch 2 commit", treeId, [root]);

    // mergeBase
    const base = repo.mergeBase(b1, b2);
    expect(base).toBe(root);

    // mergeBaseOctopus
    const octopusBase = repo.mergeBaseOctopus([b1, b2]);
    expect(octopusBase).toBe(root);

    // isAncestor
    expect(repo.isAncestor(root, b1)).toBe(true);
    expect(repo.isAncestor(root, b2)).toBe(true);
    expect(repo.isAncestor(b1, b2)).toBe(false);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

// --- Wave 7: Mailmap ----------------------------------------------------------

test("Wave 7: mailmapResolve", () => {
  const repo = openRepo();
  const identity = repo.mailmapResolve("Yumin Chen", "yumin@chen.software");
  expect(identity).toBeDefined();
  expect(typeof identity.name).toBe("string");
  expect(typeof identity.email).toBe("string");
});

// --- Wave 8: Submodules, State, Identity, Options ------------------------------

test("Wave 8: submodules, state, committer, emptyTreeId", () => {
  const repo = openRepo();

  // submodules
  const submodules = repo.submodules();
  expect(Array.isArray(submodules)).toBe(true);

  // state
  const state = repo.state();
  expect(typeof state).toBe("string");

  // committer
  const committer = repo.committer();
  if (committer !== null) {
    expect(typeof committer.name).toBe("string");
    expect(typeof committer.email).toBe("string");
    expect(typeof committer.timeSeconds).toBe("number");
  }

  // emptyTreeId
  const emptyTreeId = repo.emptyTreeId();
  expect(typeof emptyTreeId).toBe("string");
  expect(emptyTreeId.length).toBe(40);
  expect(emptyTreeId).toBe("4b825dc642cb6eb9a060e54bf8d69288fbee4904");
});

// --- 6. Tree & object inspection ---------------------------------------------

test("head().tree() returns the current HEAD tree with entries", () => {
  const repo = openRepo();
  const tree = repo.head().tree();

  expect(typeof tree.id).toBe("string");
  expect(tree.id.length).toBeGreaterThan(0);
  expect(Array.isArray(tree.entries)).toBe(true);
  expect(tree.entries.length).toBeGreaterThan(0);

  for (const e of tree.entries) {
    expect(typeof e.name).toBe("string");
    expect(e.name.length).toBeGreaterThan(0);
    expect(typeof e.id).toBe("string");
    expect(typeof e.mode).toBe("string");
  }

  console.log(`\nHEAD tree ${tree.id.slice(0, 12)}:\n` + renderTree(tree));
});

test("headCommit().tree() matches head().tree()", () => {
  const repo = openRepo();
  const viaHead = repo.head().tree();
  const viaCommit = repo.headCommit().tree();

  expect(viaCommit.id).toBe(viaHead.id);
  expect(viaCommit.entries.length).toBe(viaHead.entries.length);
});

test("revParseSingle('HEAD^{tree}') resolves to the same tree", () => {
  const repo = openRepo();
  const treeId = repo.revParseSingle("HEAD^{tree}");
  const tree = repo.findTree(treeId);

  expect(tree.id).toBe(repo.head().tree().id);
});

test("tree.entry() finds a direct child by name", () => {
  const repo = openRepo();
  const tree = repo.head().tree();

  const first = tree.entries[0]!;
  const found = tree.entry(first.name);
  expect(found).not.toBeUndefined();
  expect(found!.id).toBe(first.id);

  expect(tree.entry("__does_not_exist__")).toBeNull();
});

test("recurse into a nested tree (subdirectory)", () => {
  const repo = openRepo();
  const tree = repo.head().tree();

  const dir = tree.entries.find((e) => e.mode === "Tree");
  if (dir) {
    const sub = repo.findTree(dir.id);
    expect(Array.isArray(sub.entries)).toBe(true);
    console.log(`\nSubdirectory "${dir.name}" tree ${sub.id.slice(0, 12)}:\n` + renderTree(sub));
  } else {
    console.log("\nHEAD tree has no subdirectory entries.");
  }
});
