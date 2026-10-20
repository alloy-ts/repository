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
    repo = await Repository.clone(realRoot, cloneDir, false);
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
  test("blame() returns ordered, non-overlapping per-hunk entries", () => {
    const outcome = repo.blame(file);
    expect(Array.isArray(outcome.entries)).toBe(true);
    expect(outcome.entries.length).toBeGreaterThan(0);

    let prevStart = 0;
    let covered = 0;
    for (const e of outcome.entries) {
      expect(typeof e.commitId).toBe("string");
      expect(e.commitId.length).toBe(40);
      expect(e.len).toBeGreaterThan(0);
      expect(e.startInBlamedFile).toBeGreaterThanOrEqual(prevStart);
      // An unrenamed file exposes no source file name.
      expect(e.sourceFileName).toBe(null);
      prevStart = e.startInBlamedFile + e.len;
      covered += e.len;
    }
    expect(covered).toBeGreaterThan(0);

    // The blamed file content is returned as bytes.
    expect((outcome.blob as unknown as { length: number }).length).toBeGreaterThan(0);
  });

  test("blame() populates statistics and keeps blamePath null", () => {
    const outcome = repo.blame(file);
    expect(outcome.statistics.commitsTraversed).toBeGreaterThan(0);
    expect(outcome.statistics.treesDecoded).toBeGreaterThan(0);
    expect(outcome.statistics.treesDiffed).toBeGreaterThan(0);
    expect(outcome.statistics.blobsDiffed).toBeGreaterThan(0);
    // debug_track_path is forced off by the high-level wrapper, so this is always null.
    expect(outcome.blamePath).toBe(null);
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
      // startInBlamedFile / len are 0-based; the 1-based inclusive last line is start + len.
      expect(e.startInBlamedFile + 1).toBeGreaterThanOrEqual(ranges[0].start);
      expect(e.startInBlamedFile + e.len).toBeLessThanOrEqual(ranges[0].end);
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
});
