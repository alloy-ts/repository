import { execSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, unlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Algorithm, Repository } from "./main.ts";

let fixture: string;
let repo: Repository;
// `c1`/`c2` bracket a commit that touches every interesting case: a text file
// with a function-context header, a two-hunk file, a file without a trailing
// newline, an added file, a deleted file and a binary file.
let c1: string;
let c2: string;
let c1Tree: string;
let c2Tree: string;

const SRC_V1 =
  [
    "#include <stdio.h>",
    "",
    "static int helper(int x) {",
    "\treturn x + 1;",
    "}",
    "",
    "int main(void) {",
    "\tint a = 1;",
    "\tint b = 2;",
    "\tint c = 3;",
    "\tint d = 4;",
    '\tprintf("%d\\n", a + b);',
    "\treturn 0;",
    "}",
  ].join("\n") + "\n";
const SRC_V2 = SRC_V1.replace("\tint b = 2;", "\tint b = 22;");
const LINES_V1 = Array.from({ length: 10 }, (_, i) => `line${i + 1}`).join("\n") + "\n";
const LINES_V2 = LINES_V1.replace("line1\n", "LINE1\n").replace("line10", "LINE10");

function sh(command: string): string {
  return execSync(command, { cwd: fixture }).toString();
}

function git(args: string) {
  execSync(`git ${args}`, { cwd: fixture, stdio: "ignore" });
}

function put(name: string, content: string | Buffer) {
  writeFileSync(join(fixture, name), content);
}

/** Blob id of `path` at `rev`, or `null` when the path does not exist there. */
function blob(rev: string, path: string): string | null {
  try {
    return execSync(`git rev-parse ${rev}:${path}`, {
      cwd: fixture,
      stdio: ["ignore", "pipe", "ignore"],
    })
      .toString()
      .trim();
  } catch {
    return null;
  }
}

/**
 * `git diff` for a single path, pinned against user configuration that would
 * otherwise change the header shape (prefixes, abbreviation width, algorithm).
 */
function gitDiff(range: string, path: string, extra = ""): string {
  return sh(
    `git -c diff.noprefix=false -c diff.mnemonicPrefix=false -c diff.renames=false ` +
      `-c diff.algorithm=histogram -c core.abbrev=7 ` +
      `diff ${extra} --no-color --no-ext-diff ${range} -- ${path}`,
  );
}

beforeAll(() => {
  fixture = mkdtempSync(join(tmpdir(), "repo-patch-"));
  git("init --initial-branch=main");
  git("config user.email test@example.com");
  git("config user.name Test");
  git("config commit.gpgsign false");

  put("src.c", SRC_V1);
  put("lines.txt", LINES_V1);
  put("noeol.txt", "first\nlast-without-newline");
  put("gone.txt", "will be deleted\n");
  put("bin.dat", Buffer.from([0x00, 0x01, 0x02, 0x00]));
  git("add -A");
  git("commit -m one");
  c1 = sh("git rev-parse HEAD").trim();
  c1Tree = sh(`git rev-parse ${c1}^{tree}`).trim();

  put("src.c", SRC_V2);
  put("lines.txt", LINES_V2);
  put("noeol.txt", "first\nlast-without-newline-changed");
  put("new.txt", "brand new\nsecond line\n");
  unlinkSync(join(fixture, "gone.txt"));
  put("bin.dat", Buffer.from([0x00, 0x01, 0x03, 0x00]));
  git("add -A");
  git("commit -m two");
  c2 = sh("git rev-parse HEAD").trim();
  c2Tree = sh(`git rev-parse ${c2}^{tree}`).trim();

  repo = Repository.open(fixture);
});

afterAll(() => {
  rmSync(fixture, { recursive: true, force: true });
});

describe("Patch construction (src/patch.rs)", () => {
  test("patchFromBuffers exposes hunks, lines and stats for a one-line edit", () => {
    const patch = repo.patchFromBuffers(
      Buffer.from("a\nb\nc\n"),
      "old.txt",
      Buffer.from("a\nB\nc\n"),
      "new.txt",
    );

    expect(patch.numHunks()).toBe(1);
    expect(patch.hunk(0)).toEqual({
      oldStart: 1,
      oldLines: 3,
      newStart: 1,
      newLines: 3,
      header: "@@ -1,3 +1,3 @@",
    });
    expect(patch.lineStats()).toEqual({
      context: 2,
      additions: 1,
      deletions: 1,
    });
    expect(patch.numLinesInHunk(0)).toBe(4);

    expect(patch.lineInHunk(0, 0)).toEqual({
      origin: " ",
      content: "a\n",
      oldLineno: 1,
      newLineno: 1,
      noNewline: false,
    });
    expect(patch.lineInHunk(0, 1)).toEqual({
      origin: "-",
      content: "b\n",
      oldLineno: 2,
      noNewline: false,
    });
    expect(patch.lineInHunk(0, 2)).toEqual({
      origin: "+",
      content: "B\n",
      newLineno: 2,
      noNewline: false,
    });

    expect(patch.isBinary()).toBe(false);
    expect(patch.delta()).toEqual({
      oldPath: "old.txt",
      newPath: "new.txt",
      oldMode: "100644",
      newMode: "100644",
      status: "Modified",
      binary: false,
    });
    // Buffers carry no blob ids, so the `index` line is omitted.
    expect(patch.toBuf()).toBe(
      "diff --git a/old.txt b/new.txt\n" +
        "--- a/old.txt\n" +
        "+++ b/new.txt\n" +
        "@@ -1,3 +1,3 @@\n" +
        " a\n" +
        "-b\n" +
        "+B\n" +
        " c\n",
    );
  });

  test("patchFromBuffers reports function context in hunk headers", () => {
    const patch = repo.patchFromBuffers(Buffer.from(SRC_V1), "src.c", Buffer.from(SRC_V2), "src.c");
    expect(patch.numHunks()).toBe(1);
    expect(patch.hunk(0).header).toBe("@@ -6,7 +6,7 @@ static int helper(int x) {");
  });

  test("patchFromBuffers honours contextLines", () => {
    const wholeFile = Buffer.from(
      Array.from({ length: 10 }, (_, i) => `line${i + 1}`).join("\n") + "\n",
    );
    const edited = Buffer.from(wholeFile.toString().replace("line5\n", "LINE5\n"));

    expect(
      repo.patchFromBuffers(wholeFile, "a", edited, "a", { contextLines: 0 }).toBuf(),
    ).toContain("@@ -5 +5 @@");
    expect(
      repo.patchFromBuffers(wholeFile, "a", edited, "a", { contextLines: 1 }).hunk(0),
    ).toMatchObject({
      // `line3` is the nearest identifier-ish line before the hunk, like git.
      header: "@@ -4,3 +4,3 @@ line3",
      oldLines: 3,
      newLines: 3,
    });
    expect(repo.patchFromBuffers(wholeFile, "a", edited, "a").numLinesInHunk(0)).toBe(8);

    const myers = repo.patchFromBuffers(wholeFile, "a", edited, "a", {
      algorithm: Algorithm.Myers,
    });
    expect(myers.toBuf()).toBe(repo.patchFromBuffers(wholeFile, "a", edited, "a").toBuf());
  });

  test("patchFromBuffers marks identical content as Unmodified and renders nothing", () => {
    const patch = repo.patchFromBuffers(Buffer.from("same\n"), "a", Buffer.from("same\n"), "a");
    expect(patch.delta().status).toBe("Unmodified");
    expect(patch.numHunks()).toBe(0);
    expect(patch.toBuf()).toBe("");
    expect(patch.size()).toBe(0);
  });

  test("patchFromBuffers marks an empty pre-image as an addition", () => {
    const patch = repo.patchFromBuffers(
      Buffer.alloc(0),
      "new.txt",
      Buffer.from("one\ntwo\n"),
      "new.txt",
    );
    expect(patch.delta().status).toBe("Added");
    expect(patch.hunk(0)).toMatchObject({
      oldStart: 0,
      oldLines: 0,
      newStart: 1,
      newLines: 2,
    });
    const text = patch.toBuf();
    expect(text).toContain("new file mode 100644\n");
    expect(text).toContain("--- /dev/null\n+++ b/new.txt\n");
  });

  test("binary content yields no hunks and the git binary notice", () => {
    const patch = repo.patchFromBuffers(
      Buffer.from([0x00, 0x01]),
      "bin",
      Buffer.from([0x00, 0x02]),
      "bin",
    );
    expect(patch.isBinary()).toBe(true);
    expect(patch.delta().binary).toBe(true);
    expect(patch.numHunks()).toBe(0);
    expect(patch.toBuf()).toContain("Binary files a/bin and b/bin differ\n");
    // Binary patches carry no pre/post images.
    expect(patch.toBuf()).not.toContain("--- a/bin");

    // `text: true` forces a line diff even through NUL bytes.
    const forced = repo.patchFromBuffers(Buffer.from("a\n"), "bin", Buffer.from("b\n"), "bin", {
      text: true,
    });
    expect(forced.isBinary()).toBe(false);
    expect(forced.numHunks()).toBe(1);
  });

  test("hunk and line accessors throw when out of range", () => {
    const patch = repo.patchFromBuffers(Buffer.from("a\n"), "a", Buffer.from("b\n"), "a");
    expect(() => patch.hunk(1)).toThrow();
    expect(() => patch.numLinesInHunk(1)).toThrow();
    expect(() => patch.lineInHunk(0, 99)).toThrow();
    expect(() => patch.lineInHunk(9, 0)).toThrow();
  });

  test("size() reports the byte length of the matching rendering", () => {
    const patch = repo.patchFromBlobs(blob(c1, "src.c")!, "src.c", blob(c2, "src.c")!, "src.c");
    expect(patch.size()).toBe(Buffer.byteLength(patch.toBuf(), "utf8"));
    expect(patch.size(false)).toBe(
      Buffer.byteLength(patch.toBuf({ includeContext: false }), "utf8"),
    );
    expect(patch.size(true, true, false)).toBe(
      Buffer.byteLength(patch.toBuf({ includeFileHeaders: false }), "utf8"),
    );
    expect(patch.size(true, false, true)).toBe(
      Buffer.byteLength(patch.toBuf({ includeHunkHeaders: false }), "utf8"),
    );
    // Dropping context and headers must shrink the rendering.
    expect(patch.size(false, false, false)).toBeLessThan(patch.size());
  });
});

describe("Patch output parity with git (src/patch.rs)", () => {
  test.each([
    ["src.c", "modified file with a function context"],
    ["lines.txt", "modified file with two hunks"],
    ["noeol.txt", "modified file without a trailing newline"],
    ["new.txt", "added file"],
    ["gone.txt", "deleted file"],
    ["bin.dat", "modified binary file"],
  ])("patchFromBlobs reproduces `git diff` for %s (%s)", (path) => {
    const patch = repo.patchFromBlobs(blob(c1, path), path, blob(c2, path), path);
    expect(patch.toBuf()).toBe(gitDiff(`${c1} ${c2}`, path));
  });

  test("no-newline files render the `\\ No newline at end of file` marker", () => {
    const patch = repo.patchFromBlobs(
      blob(c1, "noeol.txt")!,
      "noeol.txt",
      blob(c2, "noeol.txt")!,
      "noeol.txt",
    );
    expect(patch.toBuf()).toContain("\\ No newline at end of file\n");
    expect(patch.lineInHunk(0, patch.numLinesInHunk(0) - 1).noNewline).toBe(true);
  });

  test("hunk headers carry git's function context", () => {
    const patch = repo.patchFromBlobs(blob(c1, "src.c")!, "src.c", blob(c2, "src.c")!, "src.c");
    const expectedHeader = gitDiff(`${c1} ${c2}`, "src.c")
      .split("\n")
      .find((line) => line.startsWith("@@"))!;
    expect(patch.hunk(0).header).toBe(expectedHeader);
  });

  test.each([0, 1, 5])("contextLines %i matches `git diff -U%i`", (contextLines) => {
    const patch = repo.patchFromBlobs(
      blob(c1, "lines.txt")!,
      "lines.txt",
      blob(c2, "lines.txt")!,
      "lines.txt",
      { contextLines },
    );
    expect(patch.toBuf()).toBe(gitDiff(`${c1} ${c2}`, "lines.txt", `-U${contextLines}`));
  });

  test("delta() reports paths, ids, modes and status", () => {
    const patch = repo.patchFromBlobs(blob(c1, "src.c")!, "src.c", blob(c2, "src.c")!, "src.c");
    expect(patch.delta()).toEqual({
      oldPath: "src.c",
      newPath: "src.c",
      oldId: blob(c1, "src.c"),
      newId: blob(c2, "src.c"),
      oldMode: "100644",
      newMode: "100644",
      status: "Modified",
      binary: false,
    });
  });
});

describe("Patch sources (src/patch.rs)", () => {
  test("patchFromBlobAndBuffer diffs a blob against raw content", () => {
    const patch = repo.patchFromBlobAndBuffer(
      blob(c1, "src.c")!,
      "src.c",
      Buffer.from(SRC_V2),
      "src.c",
    );
    const fromBlobs = repo.patchFromBlobs(blob(c1, "src.c"), "src.c", blob(c2, "src.c"), "src.c");

    expect(patch.delta().oldId).toBe(blob(c1, "src.c"));
    // The buffer has no blob id, so no `index` line is emitted.
    expect(patch.delta().newId).toBeUndefined();
    expect(patch.toBuf()).not.toContain("index ");
    // The hunk bodies are identical either way.
    expect(patch.toBuf({ includeFileHeaders: false })).toBe(
      fromBlobs.toBuf({ includeFileHeaders: false }),
    );
  });

  test("patchFromChange mirrors Patch::from_diff and matches the blob path", () => {
    const changes = repo.diffTreeToTree(c1Tree, c2Tree);
    const change = changes.find((c) => c.path === "src.c")!;
    const fromChange = repo.patchFromChange(change)!;
    const fromBlobs = repo.patchFromBlobs(blob(c1, "src.c"), "src.c", blob(c2, "src.c"), "src.c");

    expect(fromChange.delta()).toMatchObject({
      status: "Modified",
      oldPath: "src.c",
    });
    expect(fromChange.toBuf()).toBe(fromBlobs.toBuf());
  });

  test("patchFromChange returns null when the change carries no content diff", () => {
    const id = blob(c2, "src.c")!;
    const unchanged = repo.patchFromChange({
      path: "src.c",
      kind: "Modified",
      entryMode: "100644",
      oldId: id,
      newId: id,
    });
    expect(unchanged).toBeNull();
  });

  test("patchFromChange covers added and deleted entries", () => {
    const changes = repo.diffTreeToTree(c1Tree, c2Tree);
    const added = repo.patchFromChange(changes.find((c) => c.path === "new.txt")!);
    const deleted = repo.patchFromChange(changes.find((c) => c.path === "gone.txt")!);

    expect(added!.delta().status).toBe("Added");
    expect(added!.toBuf()).toBe(gitDiff(`${c1} ${c2}`, "new.txt"));
    expect(deleted!.delta().status).toBe("Deleted");
    expect(deleted!.toBuf()).toBe(gitDiff(`${c1} ${c2}`, "gone.txt"));
  });
});

describe("formatPatch (src/patch.rs)", () => {
  test("formatPatchToString renders an mbox with From/Date/Subject and the diff", () => {
    const text = repo.formatPatchToString("HEAD~1..HEAD");
    expect(text).toContain("From: ");
    expect(text).toContain("Date: ");
    expect(text).toContain("Subject: [PATCH] ");
    expect(text).toContain("---");
    expect(text).toContain("+++");
  });

  test("formatPatch writes one numbered patch file and returns its path", () => {
    const paths = repo.formatPatch("HEAD~1..HEAD", { numbered: true });
    expect(paths.length).toBe(1);
    expect(paths[0]).toMatch(/0001-.*\.patch$/);
    const written = readFileSync(paths[0], "utf8");
    expect(written).toContain("Subject: [PATCH 1/1] ");
  });

  test("signoff appends a Signed-off-by trailer", () => {
    const text = repo.formatPatchToString("HEAD~1..HEAD", { signoff: true });
    expect(text).toContain("Signed-off-by: ");
  });
});
