import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";

import {
  ConflictKind,
  FileStatus,
  Repository,
  conflictKindCode,
  fileStatusIsStaged,
  fileStatusIsUnstaged,
} from "./main.ts";

// Mirrors `StatusEntry` from status.rs. Declared locally so this file does not
// depend on a freshly rebuilt dist/index.d.ts.
interface StatusEntry {
  path: string;
  oldPath: string | null;
  index: string | null;
  worktree: string | null;
  porcelain: string;
  directory: boolean;
}

let fixture: string;
let repo: Repository;

const RENAMED_CONTENT = Array.from({ length: 12 }, (_, i) => `rename line ${i}`).join("\n") + "\n";
const COPIED_CONTENT = Array.from({ length: 12 }, (_, i) => `copy line ${i}`).join("\n") + "\n";

function git(args: string, opts: { allowFailure?: boolean } = {}) {
  const r = spawnSync("git", args.split(" "), {
    cwd: fixture,
    stdio: "ignore",
  });
  if (r.status !== 0 && !opts.allowFailure) {
    throw new Error(`git ${args} failed with status ${r.status}`);
  }
}

function write(rel: string, content: string) {
  writeFileSync(join(fixture, rel), content);
}

function remove(rel: string) {
  rmSync(join(fixture, rel), { force: true, recursive: true });
}

/** Restore the fixture to a clean tracked tree (drops untracked + ignored files too). */
function resetHard() {
  git("reset -q --hard HEAD");
  git("clean -qfdx");
}

function find(list: StatusEntry[], path: string): StatusEntry {
  const e = list.find((x) => x.path === path);
  expect(
    e,
    `expected a status entry for ${path}, got ${JSON.stringify(list.map((x) => x.path))}`,
  ).toBeDefined();
  return e!;
}

beforeAll(() => {
  fixture = mkdtempSync(join(tmpdir(), "repository-status-"));
  git("init --initial-branch=main");
  git("config user.email test@example.com");
  git("config user.name Test");
  git("config commit.gpgsign false");
  // Pin the untracked default so plain `status()` calls are deterministic
  // (gix falls back to `collapsed` when `status.showUntrackedFiles` is unset).
  git("config status.showUntrackedFiles all");

  write("a.txt", "a\n");
  write("b.txt", "b\n");
  mkdirSync(join(fixture, "dir"));
  write("dir/nested.txt", "nested\n");
  write("rename-source.txt", RENAMED_CONTENT);
  write("copy-source.txt", COPIED_CONTENT);
  write("conflict.txt", "base\n");
  write("typechange.txt", "file\n");
  write(".gitignore", "ignored/\n*.log\n");
  git("add -A");
  git("commit -qm init");

  repo = Repository.open(fixture);
});

afterAll(() => {
  rmSync(fixture, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 200,
  });
});

// ---------------------------------------------------------------------------
// Clean tree, untracked and ignored files
// ---------------------------------------------------------------------------

describe("Repository.status — clean tree, untracked and ignored", () => {
  // Must run before any test mutates the fixture.
  test("a clean repository reports no entries and is not dirty", async () => {
    await expect(repo.status()).resolves.toEqual([]);
    await expect(repo.isDirty()).resolves.toBe(false);
  });

  test("untracked files are reported as '??' entries", async () => {
    write("u1.txt", "u1\n");
    const entries = await repo.status();
    const e = find(entries, "u1.txt");
    expect(e.index).toBe(null);
    expect(e.worktree).toBe("untracked");
    expect(e.porcelain).toBe("??");
    expect(e.directory).toBe(false);
    expect(e.oldPath).toBe(null);
    remove("u1.txt");
  });

  test("untracked directories collapse to one entry by default settings, expand with 'files'", async () => {
    mkdirSync(join(fixture, "newdir"));
    write("newdir/x.txt", "x\n");
    write("newdir/y.txt", "y\n");

    const collapsed = await repo.status({ untracked: "collapsed" });
    const dir = find(collapsed, "newdir");
    expect(dir.directory).toBe(true);
    expect(dir.porcelain).toBe("??");
    expect(collapsed.filter((e) => e.path.startsWith("newdir")).length).toBe(1);

    const expanded = await repo.status({ untracked: "files" });
    const paths = expanded.map((e) => e.path).filter((p) => p.startsWith("newdir"));
    expect(paths.sort()).toEqual(["newdir/x.txt", "newdir/y.txt"]);
    for (const e of expanded.filter((x) => x.path.startsWith("newdir"))) {
      expect(e.directory).toBe(false);
    }
    remove("newdir");
  });

  test("ignored files are hidden by default and shown as '!!' with ignored: 'matching'", async () => {
    write("noise.log", "noise\n");
    const quiet = await repo.status({ untracked: "files" });
    expect(quiet.find((e) => e.path === "noise.log")).toBeUndefined();

    const loud = await repo.status({ untracked: "files", ignored: "matching" });
    const e = find(loud, "noise.log");
    expect(e.worktree).toBe("ignored");
    expect(e.index).toBe(null);
    expect(e.porcelain).toBe("!!");
    remove("noise.log");
  });

  test("entries are sorted by path", async () => {
    write("zz.txt", "zz\n");
    write("aa.txt", "aa\n");
    const paths = (await repo.status({ untracked: "files" }))
      .map((e) => e.path)
      .filter((p) => p === "aa.txt" || p === "zz.txt");
    expect(paths).toEqual(["aa.txt", "zz.txt"]);
    remove("zz.txt");
    remove("aa.txt");
  });

  test("untracked files do not make the repository dirty", async () => {
    write("u2.txt", "u2\n");
    await expect(repo.isDirty()).resolves.toBe(false);
    remove("u2.txt");
  });
});

// ---------------------------------------------------------------------------
// Tracked modifications: index (staged) and worktree (unstaged) sides
// ---------------------------------------------------------------------------

describe("Repository.status — staged and unstaged changes", () => {
  test("unstaged modification: ' M' with a worktree side only", async () => {
    write("b.txt", "changed\n");
    const e = find(await repo.status(), "b.txt");
    expect(e.index).toBe(null);
    expect(e.worktree).toBe("modified");
    expect(e.porcelain).toBe(" M");
    expect(e.directory).toBe(false);
    await expect(repo.isDirty()).resolves.toBe(true);
    resetHard();
  });

  test("staged modification: 'M ' with an index side only", async () => {
    write("b.txt", "staged\n");
    git("add b.txt");
    const e = find(await repo.status(), "b.txt");
    expect(e.index).toBe("modified");
    expect(e.worktree).toBe(null);
    expect(e.porcelain).toBe("M ");
    resetHard();
  });

  test("staged and unstaged modification: 'MM'", async () => {
    write("b.txt", "staged\n");
    git("add b.txt");
    write("b.txt", "unstaged\n");
    const e = find(await repo.status(), "b.txt");
    expect(e.index).toBe("modified");
    expect(e.worktree).toBe("modified");
    expect(e.porcelain).toBe("MM");
    resetHard();
  });

  test("unstaged deletion: ' D'", async () => {
    remove("b.txt");
    const e = find(await repo.status(), "b.txt");
    expect(e.worktree).toBe("deleted");
    expect(e.index).toBe(null);
    expect(e.porcelain).toBe(" D");
    resetHard();
  });

  test("staged deletion: 'D '", async () => {
    git("rm -q b.txt");
    const e = find(await repo.status(), "b.txt");
    expect(e.index).toBe("deleted");
    expect(e.worktree).toBe(null);
    expect(e.porcelain).toBe("D ");
    resetHard();
  });

  test("staged addition: 'A '", async () => {
    write("added.txt", "added\n");
    git("add added.txt");
    const e = find(await repo.status(), "added.txt");
    expect(e.index).toBe("added");
    expect(e.worktree).toBe(null);
    expect(e.porcelain).toBe("A ");
    resetHard();
  });

  test("file replaced by a symlink is a typechange: ' T'", async () => {
    remove("typechange.txt");
    symlinkSync("a.txt", join(fixture, "typechange.txt"));
    const e = find(await repo.status(), "typechange.txt");
    expect(e.worktree).toBe("typechange");
    expect(e.porcelain).toBe(" T");
    resetHard();
  });

  test("intent-to-add file: ' A', and 'AM' once its content changes", async () => {
    write("i2a.txt", "initial\n");
    git("add -N i2a.txt");
    let e = find(await repo.status(), "i2a.txt");
    expect(e.index).toBe(null);
    expect(e.worktree).toBe("intent-to-add");
    expect(e.porcelain).toBe(" A");

    write("i2a.txt", "initial\nand more\n");
    e = find(await repo.status(), "i2a.txt");
    expect(e.index).toBe("added");
    expect(e.worktree).toBe("modified");
    expect(e.porcelain).toBe("AM");
    remove("i2a.txt");
    resetHard();
  });

  test("show: 'index' drops worktree-only halves", async () => {
    write("b.txt", "unstaged only\n");
    const unstagedOnly = await repo.status({
      show: "index",
      untracked: "none",
    });
    expect(unstagedOnly.find((e) => e.path === "b.txt")).toBeUndefined();

    git("add b.txt");
    const staged = await repo.status({ show: "index", untracked: "none" });
    const e = find(staged, "b.txt");
    expect(e.index).toBe("modified");
    expect(e.worktree).toBe(null);
    resetHard();
  });
});

// ---------------------------------------------------------------------------
// Renames and copies between HEAD and the index
// ---------------------------------------------------------------------------

describe("Repository.status — renames and copies", () => {
  test("staged rename is detected with explicit rewrites: 'R ' + oldPath", async () => {
    git("mv rename-source.txt renamed-dest.txt");
    const entries = await repo.status({
      untracked: "none",
      headToIndexRenames: { percentage: 50 },
    });
    expect(entries.length).toBe(1);
    const e = entries[0];
    expect(e.path).toBe("renamed-dest.txt");
    expect(e.oldPath).toBe("rename-source.txt");
    expect(e.index).toBe("renamed");
    expect(e.worktree).toBe(null);
    expect(e.porcelain).toBe("R ");

    // Rename detection disabled: the same state is an add + a delete.
    const noRenames = await repo.status({
      untracked: "none",
      headToIndexRenames: "none",
    });
    expect(find(noRenames, "renamed-dest.txt").porcelain).toBe("A ");
    expect(find(noRenames, "rename-source.txt").porcelain).toBe("D ");
    resetHard();
  });

  test("staged duplicate content is reported as a copy with copies enabled", async () => {
    write("copy-dest.txt", COPIED_CONTENT);
    git("add copy-dest.txt");
    const entries = await repo.status({
      untracked: "none",
      headToIndexRenames: { percentage: 50, copies: true },
    });
    const e = find(entries, "copy-dest.txt");
    expect(e.index).toBe("copied");
    expect(e.oldPath).toBe("copy-source.txt");
    expect(e.porcelain).toBe("C ");
    resetHard();
  });
});

// ---------------------------------------------------------------------------
// head tree-ish and pathspec options
// ---------------------------------------------------------------------------

describe("Repository.status — head and pathspec options", () => {
  test("head: diffs the index against the given tree-ish instead of HEAD", async () => {
    // Commit v2, then stage v1's content: modified against HEAD, clean against HEAD~1.
    write("b.txt", "b v2\n");
    git("add b.txt");
    git("commit -qm v2");
    write("b.txt", "b\n");
    git("add b.txt");

    await expect(repo.status({ head: "HEAD~1", untracked: "none" })).resolves.toEqual([]);
    const e = find(await repo.status({ untracked: "none" }), "b.txt");
    expect(e.porcelain).toBe("M ");
    resetHard();
  });

  test("pathspec restricts the report to matching paths", async () => {
    write("b.txt", "modified\n");
    write("zz.txt", "untracked\n");
    const entries = await repo.status({
      pathspec: ["b.txt"],
      untracked: "files",
    });
    expect(entries.map((e) => e.path)).toEqual(["b.txt"]);
    remove("zz.txt");
    resetHard();
  });

  test("disablePathspecMatch treats pathspec entries as literal paths", async () => {
    write("lit?x.txt", "literal\n");
    write("litqx.txt", "globmatch\n");

    // As a glob, "lit?x.txt" matches both files.
    const globbed = await repo.status({
      pathspec: ["lit?x.txt"],
      untracked: "files",
    });
    expect(globbed.map((e) => e.path).sort()).toEqual(["lit?x.txt", "litqx.txt"]);

    // As a literal path, only the file whose name contains '?' matches.
    const literal = await repo.status({
      pathspec: ["lit?x.txt"],
      disablePathspecMatch: true,
      untracked: "files",
    });
    expect(literal.map((e) => e.path)).toEqual(["lit?x.txt"]);
    remove("lit?x.txt");
    remove("litqx.txt");
  });
});

// ---------------------------------------------------------------------------
// indexWorktree, statusFile, shouldIgnore
// ---------------------------------------------------------------------------

describe("Repository.indexWorktree / statusFile / shouldIgnore", () => {
  test("indexWorktree ignores staged-only changes", async () => {
    write("b.txt", "staged\n");
    git("add b.txt");
    await expect(repo.indexWorktree()).resolves.toEqual([]);
    resetHard();
  });

  test("indexWorktree returns worktree halves only, even when show: 'index' is set", async () => {
    write("b.txt", "unstaged\n");
    const entries = await repo.indexWorktree({ show: "index" });
    const e = find(entries, "b.txt");
    expect(e.index).toBe(null);
    expect(e.worktree).toBe("modified");
    expect(e.porcelain).toBe(" M");
    resetHard();
  });

  test("statusFile returns null for clean and unknown paths", async () => {
    await expect(repo.statusFile("dir/nested.txt")).resolves.toBe(null);
    await expect(repo.statusFile("no-such-path.txt")).resolves.toBe(null);
  });

  test("statusFile reports modified and untracked paths", async () => {
    write("b.txt", "modified\n");
    const modified = await repo.statusFile("b.txt");
    expect(modified).not.toBe(null);
    expect(modified!.porcelain).toBe(" M");

    write("sf-untracked.txt", "?\n");
    const untracked = await repo.statusFile("sf-untracked.txt");
    expect(untracked).not.toBe(null);
    expect(untracked!.porcelain).toBe("??");
    expect(untracked!.worktree).toBe("untracked");
    remove("sf-untracked.txt");
    resetHard();
  });

  test("shouldIgnore consults .gitignore rules synchronously", () => {
    mkdirSync(join(fixture, "ignored"), { recursive: true });
    expect(repo.shouldIgnore("ignored/x")).toBe(true);
    expect(repo.shouldIgnore("fresh.log")).toBe(true);
    expect(repo.shouldIgnore("b.txt")).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// Conflicts
// ---------------------------------------------------------------------------

describe("Repository.status — merge conflicts", () => {
  test("a both-modified conflict is reported as 'UU'", async () => {
    git("checkout -qb feature");
    write("conflict.txt", "feature\n");
    git("commit -aqm feature");
    git("checkout -q main");
    write("conflict.txt", "main\n");
    git("commit -aqm main");
    git("merge feature", { allowFailure: true }); // exits non-zero on conflict

    const e = find(await repo.status({ untracked: "none" }), "conflict.txt");
    expect(e.porcelain).toBe("UU");
    expect(e.index).toBe(null);
    expect(e.worktree).toBe(null);

    git("merge --abort");
    git("branch -D feature");
    resetHard();
  });
});

// ---------------------------------------------------------------------------
// Option validation and index refresh
// ---------------------------------------------------------------------------

describe("Repository.status — option validation", () => {
  test("invalid enum values reject with a helpful error", async () => {
    await expect(repo.status({ show: "bogus" } as never)).rejects.toThrow(/show/);
    await expect(repo.status({ untracked: "bogus" } as never)).rejects.toThrow(/untracked/);
    await expect(repo.status({ ignored: "bogus" } as never)).rejects.toThrow(/ignored/);
    await expect(repo.status({ headToIndexRenames: "bogus" } as never)).rejects.toThrow(
      /headToIndexRenames/,
    );
  });

  test("updateIndex: true refreshes stat info without reporting changes", async () => {
    // Bump the mtime of an unchanged file so its index stat data is stale.
    utimesSync(join(fixture, "b.txt"), new Date(), new Date(Date.now() + 5_000));
    await expect(repo.status({ updateIndex: true, untracked: "none" })).resolves.toEqual([]);
  });

  test("fileStatusIsStaged and fileStatusIsUnstaged helper functions", () => {
    expect(fileStatusIsStaged(FileStatus.Added)).toBe(true);
    expect(fileStatusIsStaged(FileStatus.StagedModified)).toBe(true);
    expect(fileStatusIsStaged(FileStatus.StagedDeleted)).toBe(true);
    expect(fileStatusIsStaged(FileStatus.Modified)).toBe(false);
    expect(fileStatusIsStaged(FileStatus.Untracked)).toBe(false);

    expect(fileStatusIsUnstaged(FileStatus.Modified)).toBe(true);
    expect(fileStatusIsUnstaged(FileStatus.Deleted)).toBe(true);
    expect(fileStatusIsUnstaged(FileStatus.Untracked)).toBe(true);
    expect(fileStatusIsUnstaged(FileStatus.Added)).toBe(false);
  });

  test("conflictKindCode helper function", () => {
    expect(conflictKindCode(ConflictKind.BothModified)).toBe("UU");
    expect(conflictKindCode(ConflictKind.BothAdded)).toBe("AA");
    expect(conflictKindCode(ConflictKind.BothDeleted)).toBe("DD");
  });
});
