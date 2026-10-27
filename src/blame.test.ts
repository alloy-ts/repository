import { execSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-blame-"));
  cloneDir = join(base, "clone");
  try {
    repo = await Repository.clone(realRoot, cloneDir);
  } catch {
    rmSync(cloneDir, { recursive: true, force: true });
    execSync(`git clone --local --quiet ${JSON.stringify(realRoot)} ${JSON.stringify(cloneDir)}`, {
      stdio: "ignore",
    });
    repo = Repository.open(cloneDir);
  }
});

afterAll(() => {
  rmSync(base, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 200,
  });
});

// `Cargo.toml` lives at the project root and has real multi-commit history, so it
// produces several blame hunks — a good fixture for exercising the binding.
const file = "Cargo.toml";

describe("Repository.blame / blameWithOptions (src/blame.rs)", () => {
  test("blame() returns ordered, non-overlapping, 1-based per-hunk entries", () => {
    const outcome = repo.blame(file);
    expect(Array.isArray(outcome.entries)).toBe(true);
    expect(outcome.entries.length).toBeGreaterThan(0);

    let prevStart = 1;
    let covered = 0;
    for (const e of outcome.entries) {
      expect(typeof e.commitId).toBe("string");
      expect(e.commitId.length).toBe(40);
      expect(e.len).toBeGreaterThan(0);
      // 1-based start lines, monotonically non-decreasing.
      expect(e.startInBlamedFile).toBeGreaterThanOrEqual(prevStart);
      // An unrenamed file exposes no source file name (absent optional field).
      expect(e.sourceFileName).toBeUndefined();
      // Default behavior leaves commit details unresolved (napi serializes
      // absent `Option` object fields as `undefined`).
      expect(e.author).toBeUndefined();
      expect(e.committer).toBeUndefined();
      expect(e.summary).toBeUndefined();
      prevStart = e.startInBlamedFile + e.len;
      covered += e.len;
    }
    expect(covered).toBeGreaterThan(0);
    // Whole-file blame starts at line 1.
    expect(outcome.entries[0].startInBlamedFile).toBe(1);

    // The blamed file content is returned as bytes.
    expect((outcome.blob as unknown as { length: number }).length).toBeGreaterThan(0);
  });

  test("blame() populates statistics and keeps blamePath null", () => {
    const outcome = repo.blame(file);
    expect(outcome.statistics.commitsTraversed).toBeGreaterThan(0);
    expect(outcome.statistics.treesDecoded).toBeGreaterThan(0);
    expect(outcome.statistics.treesDiffed).toBeGreaterThan(0);
    expect(outcome.statistics.blobsDiffed).toBeGreaterThan(0);
    // debug_track_path is forced off by the high-level wrapper, so this is always absent.
    expect(outcome.blamePath).toBeUndefined();
  });

  test("blame() accepts an explicit commit id", () => {
    const head = repo.headCommit();
    const outcome = repo.blame(file, head.id);
    expect(outcome.entries.length).toBeGreaterThan(0);
    expect(outcome.entries[0].commitId.length).toBe(40);
  });

  test("blameWithOptions() with null options behaves like blame()", () => {
    const a = repo.blame(file);
    const b = repo.blameWithOptions(file, null, null);
    expect(b.entries.length).toBe(a.entries.length);
    expect(b.blob.length).toBe(a.blob.length);
    expect(b.statistics.commitsTraversed).toBe(a.statistics.commitsTraversed);
  });

  test("blameWithOptions() accepts an explicit diff algorithm", () => {
    for (const algo of ["myers", "minimal", "histogram"] as const) {
      const outcome = repo.blameWithOptions(file, null, {
        diffAlgorithm: algo,
      });
      expect(outcome.entries.length).toBeGreaterThan(0);
    }
  });

  test("blameWithOptions() rejects an unknown diff algorithm", () => {
    expect(() => repo.blameWithOptions(file, null, { diffAlgorithm: "patience" })).toThrow();
  });

  test("blameWithOptions() with a 1-based inclusive range blames only that region", () => {
    const full = repo.blame(file);
    const totalLines = full.entries.reduce((acc, e) => acc + e.len, 0);
    const mid = Math.max(1, Math.ceil(totalLines / 2));
    const ranges = [{ start: 1, end: mid }];

    const partial = repo.blameWithOptions(file, null, { ranges });
    expect(partial.entries.length).toBeGreaterThan(0);

    for (const e of partial.entries) {
      // 1-based: the hunk's inclusive last line is `startInBlamedFile + len - 1`.
      expect(e.startInBlamedFile).toBeGreaterThanOrEqual(ranges[0].start);
      expect(e.startInBlamedFile + e.len - 1).toBeLessThanOrEqual(ranges[0].end);
    }
  });

  test("blameWithOptions() with a 'since' cutoff does not exclude all commits", () => {
    const outcome = repo.blameWithOptions(file, null, { since: 0 });
    expect(outcome.entries.length).toBeGreaterThan(0);
  });

  test("blameWithOptions() with trackRenames blames without error", () => {
    const outcome = repo.blameWithOptions(file, null, { trackRenames: true });
    expect(outcome.entries.length).toBeGreaterThan(0);
  });

  test("blameWithOptions() with trackCopies blames without error", () => {
    const outcome = repo.blameWithOptions(file, null, { trackCopies: true });
    expect(outcome.entries.length).toBeGreaterThan(0);
  });

  test("resolveCommits populates author/committer/summary; default leaves them null", () => {
    const resolved = repo.blameWithOptions(file, null, {
      resolveCommits: true,
    });
    expect(resolved.entries.length).toBeGreaterThan(0);
    for (const e of resolved.entries) {
      expect(e.author).not.toBe(null);
      expect(typeof e.author!.name).toBe("string");
      expect(typeof e.author!.email).toBe("string");
      expect(typeof e.author!.time).toBe("number");
      expect(typeof e.author!.timezoneOffset).toBe("number");
      expect(e.committer).not.toBe(null);
      expect(typeof e.committer!.name).toBe("string");
      expect(typeof e.summary).toBe("string");
    }

    const plain = repo.blame(file);
    for (const e of plain.entries) {
      expect(e.author).toBeUndefined();
      expect(e.committer).toBeUndefined();
      expect(e.summary).toBeUndefined();
    }
  });
});

describe("Repository.blameLine (src/blame.rs)", () => {
  test("returns the hunk covering a 1-based line and null when out of range", () => {
    const all = repo.blame(file);
    const totalLines = all.entries.reduce((acc, e) => acc + e.len, 0);
    expect(totalLines).toBeGreaterThan(0);
    const mid = Math.ceil(totalLines / 2);

    const entry = repo.blameLine(file, mid, null);
    expect(entry).not.toBe(null);
    expect(mid).toBeGreaterThanOrEqual(entry!.startInBlamedFile);
    expect(mid).toBeLessThan(entry!.startInBlamedFile + entry!.len);

    // A line far beyond the file maps to no hunk.
    expect(repo.blameLine(file, totalLines + 100, null)).toBe(null);
    // Line 0 (0-based caller mistake) also maps to nothing.
    expect(repo.blameLine(file, 0, null)).toBe(null);
  });

  test("honors resolveCommits for the single returned hunk", () => {
    const all = repo.blame(file);
    const totalLines = all.entries.reduce((acc, e) => acc + e.len, 0);
    const mid = Math.ceil(totalLines / 2);
    const entry = repo.blameLine(file, mid, null, { resolveCommits: true });
    expect(entry).not.toBe(null);
    expect(entry!.author).not.toBe(null);
    expect(typeof entry!.summary).toBe("string");
  });
});
