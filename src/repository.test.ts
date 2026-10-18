import { expect, test } from "vite-plus/test";
import {
  Repository,
  type Tree,
  type TreeEntry,
} from "./main.ts";

/**
 * Draft: use the Repository API to read the *current* tree (the tree of HEAD).
 *
 * Three equivalent ways to get it:
 *   1. `repo.head().tree()`                -> tree of the current HEAD (branch/detached)
 *   2. `repo.headCommit().tree()`         -> tree of the commit HEAD points at
 *   3. `repo.findTree(repo.revParseSingle("HEAD^{tree}"))` -> by resolving the tree rev
 *
 * `Tree` exposes `tree.entries` (a getter, not a method) returning `TreeEntry[]`,
 * where each `TreeEntry` has `name`, `id` and `mode`.
 */

function openRepo(): Repository {
  // The workspace itself is a git repository, so this resolves the current repo.
  return Repository.open(process.cwd());
}

/** Render a tree as an `ls -1` style listing, with the mode and id prefix. */
function renderTree(tree: Tree): string {
  return tree.entries
    .map((e: TreeEntry) => `${e.mode.padEnd(7)} ${e.id.slice(0, 8)} ${e.name}`)
    .join("\n");
}

// --- 1. current tree via HEAD -------------------------------------------------

test("head().tree() returns the current HEAD tree with entries", () => {
  const repo = openRepo();
  const tree = repo.head().tree();

  expect(typeof tree.id).toBe("string");
  expect(tree.id.length).toBeGreaterThan(0);
  expect(Array.isArray(tree.entries)).toBe(true);
  // A non-empty repo HEAD tree has at least one entry.
  expect(tree.entries.length).toBeGreaterThan(0);

  for (const e of tree.entries) {
    expect(typeof e.name).toBe("string");
    expect(e.name.length).toBeGreaterThan(0);
    expect(typeof e.id).toBe("string"); // hex object id
    expect(typeof e.mode).toBe("string"); // e.g. "Blob", "Tree", "BlobExecutable"
  }

  console.log(`\nHEAD tree ${tree.id.slice(0, 12)}:\n` + renderTree(tree));
});

// --- 2. current tree via the head commit --------------------------------------

test("headCommit().tree() matches head().tree()", () => {
  const repo = openRepo();
  const viaHead = repo.head().tree();
  const viaCommit = repo.headCommit().tree();

  expect(viaCommit.id).toBe(viaHead.id);
  expect(viaCommit.entries.length).toBe(viaHead.entries.length);
});

// --- 3. resolve the tree rev directly -----------------------------------------

test("revParseSingle('HEAD^{tree}') resolves to the same tree", () => {
  const repo = openRepo();
  const treeId = repo.revParseSingle("HEAD^{tree}");
  const tree = repo.findTree(treeId);

  expect(tree.id).toBe(repo.head().tree().id);
});

// --- 4. look up a single entry ------------------------------------------------

test("tree.entry() finds a direct child by name", () => {
  const repo = openRepo();
  const tree = repo.head().tree();

  // Read the first entry and round-trip it through tree.entry(name).
  const first = tree.entries[0];
  const found = tree.entry(first.name);
  expect(found).not.toBeUndefined();
  expect(found!.id).toBe(first.id);

  // A non-existent name returns null (napi-rs maps Option::None to null).
  expect(tree.entry("__does_not_exist__")).toBeNull();
});

// --- 5. recurse one level to show nested trees (directories) ------------------

test("recurse into a nested tree (subdirectory)", () => {
  const repo = openRepo();
  const tree = repo.head().tree();

  // Find a child that is itself a tree (a directory) and list its contents.
  const dir = tree.entries.find((e) => e.mode === "Tree");
  if (dir) {
    const sub = repo.findTree(dir.id);
    expect(Array.isArray(sub.entries)).toBe(true);
    console.log(
      `\nSubdirectory "${dir.name}" tree ${sub.id.slice(0, 12)}:\n` +
        renderTree(sub),
    );
  } else {
    console.log("\nHEAD tree has no subdirectory entries.");
  }
});
