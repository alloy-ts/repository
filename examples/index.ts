import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { indexStatIsRacy, indexStatMatches, Repository } from "../src/main.ts";

const repo = Repository.open(process.argv[2] ?? process.cwd());

// ---------------------------------------------------------------------------
// 1. General information + entries with full stat data (read-only)
// ---------------------------------------------------------------------------
const idx = repo.openIndex();
console.log(`index at ${idx.path()}: version ${idx.version()}, ${idx.len()} entries`);
console.log(
  `sparse=${idx.isSparse()} endOfIndexMarker=${idx.hadEndOfIndexMarker()} offsetTable=${idx.hadOffsetTable()}`,
);

const ts = idx.timestamp();
console.log(`timestamp: ${ts.secs}s ${ts.nsecs}ns`);

const entries = idx.entries();
const first = entries[0];
console.log(`first entry: ${first.mode} ${first.id.slice(0, 8)} ${first.path}`);
console.log(`  stage=${first.stage} flags=0x${first.flags.toString(16)}`);
console.log(
  `  stat: size=${first.stat.size} mtime=${first.stat.mtimeSecs}.${first.stat.mtimeNsecs} ino=${first.stat.ino}`,
);

// Racy-stat check: entries modified at/after the index timestamp need re-checking.
const racyCount = entries.filter((e) => indexStatIsRacy(e.stat, ts)).length;
console.log(`racy entries (must be re-checked against disk): ${racyCount}`);

// Stat equality with git's default comparison knobs; a stat always matches itself.
console.log(`stat matches itself: ${indexStatMatches(first.stat, first.stat)}`);

// ---------------------------------------------------------------------------
// 2. Lookups
// ---------------------------------------------------------------------------
const target = first.path;
console.log(`\nentryByPath(${target}): ${idx.entryByPath(target)?.id.slice(0, 8)}`);
console.log(
  `entryByPathAndStage(${target}, 0): ${idx.entryByPathAndStage(target, 0)?.id.slice(0, 8)}`,
);
console.log(
  `entryByPathAndStage(${target}, 2) (no conflict expected): ${
    idx.entryByPathAndStage(target, 2)?.id ?? null
  }`,
);
console.log(
  `entryByPathIcase(${target.toUpperCase()}, true): ${idx
    .entryByPathIcase(target.toUpperCase(), true)
    ?.id.slice(0, 8)}`,
);

const dir = target.includes("/") ? target.slice(0, target.lastIndexOf("/")) : null;
if (dir) {
  console.log(
    `pathIsDirectory('${dir}'): ${idx.pathIsDirectory(
      dir,
    )}; icase('${dir.toUpperCase()}'): ${idx.pathIsDirectoryIcase(dir.toUpperCase(), true)}`,
  );
}
console.log(`pathIsDirectory('does-not-exist'): ${idx.pathIsDirectory("does-not-exist")}`);

// ---------------------------------------------------------------------------
// 3. Extensions
// ---------------------------------------------------------------------------
console.log(`\ntree extension: ${idx.treeExtension() ? "present" : "absent"}`);
console.log(`link (split index) extension: ${idx.linkExtension() ?? "absent"}`);
console.log(
  `untracked cache: ${
    idx.untrackedExtension() ? "present" : "absent"
  }, resolve-undo: ${idx.hasResolveUndo()}, fs-monitor: ${idx.hasFsMonitor()}`,
);

// ---------------------------------------------------------------------------
// 4. Serialization to bytes without touching the filesystem
// ---------------------------------------------------------------------------
const out = idx.writeToBuffer();
console.log(
  `\nwriteToBuffer: version=${out.version} checksum=${out.checksum?.slice(
    0,
    12,
  )} bytes=${out.data.length}`,
);
const skipped = idx.writeToBuffer(true);
console.log(
  `writeToBuffer(skipHash): version=${skipped.version} checksum=${skipped.checksum} bytes=${skipped.data.length}`,
);

// ---------------------------------------------------------------------------
// 5. Mutations on a throwaway repo: editIndex, in-memory edits, write()
// ---------------------------------------------------------------------------
const base = mkdtempSync(join(tmpdir(), "index-demo-"));
const scratchRepoPath = join(base, "scratch-repo");
// `git init` creates the directory itself; running the other commands with
// `cwd` requires it to already exist.
execSync(`git init --quiet --initial-branch=main ${JSON.stringify(scratchRepoPath)}`, {
  stdio: "ignore",
});
const git = (args: string) => execSync(`git ${args}`, { cwd: scratchRepoPath, stdio: "ignore" });
git("config user.email index@example.com");
git("config user.name Index Example");
writeFileSync(join(scratchRepoPath, "seed.txt"), "seed\n");
git("add seed.txt");
git("commit --quiet -m seed");

const clone = Repository.open(scratchRepoPath);
const blobId = clone.writeBlob(Buffer.from("written by examples/index.ts\n"));

// editIndex() upserts into the on-disk index: it appends, restores the sorted
// invariant, and drops the stale tree-cache extension before writing.
clone.editIndex([{ path: "demo/deep/file.txt", id: blobId, mode: "100644" }]);
const afterEdit = clone.openIndex();
console.log(`\nafter editIndex: ${afterEdit.len()} entries`);
console.log(
  `  entryByPath('demo/deep/file.txt'): ${afterEdit
    .entryByPath("demo/deep/file.txt")
    ?.id.slice(0, 8)}`,
);
console.log(`  pathIsDirectory('demo'): ${afterEdit.pathIsDirectory("demo")}`);

// In-memory mutation: remove an entry, sort, and persist with a skipped checksum.
const idx2 = clone.openIndex();
const removed = idx2.removeEntryAtIndex(idx2.len() - 1);
idx2.sortEntries();
console.log(`removed ${removed?.path}; writing with skipHash...`);
const version = idx2.write(true);
console.log(`write(skipHash) -> version ${version}`);

// Re-open from disk (with skipHash, since the zeroed checksum only round-trips
// through our own skipHash-aware open path) to confirm persistence.
const reopened = clone.openIndex(true);
console.log(`re-opened with skipHash: ${reopened.len()} entries`);

// Freshness timestamp (careful: a wrong timestamp causes racy-git race conditions).
reopened.setTimestamp(1_700_000_000, 0);
console.log(`setTimestamp -> ${JSON.stringify(reopened.timestamp())}`);

rmSync(base, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
console.log("\nindex example complete.");
