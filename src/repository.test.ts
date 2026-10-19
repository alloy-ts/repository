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
 * 3. Manifest Remote Setup:
 *    - repo.setupRemotesFromManifest(manifestPath?) -> setup remotes from repository.manifest.yml
 *
 * 4. Inspection of Trees and Objects.
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

// --- 3. Manifest Remote Setup -------------------------------------------------

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

// --- 4. Tree & object inspection ---------------------------------------------

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
