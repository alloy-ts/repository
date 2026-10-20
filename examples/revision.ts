import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Repository } from "../src/main.ts";

// Revision APIs: resolving revision specifications (git rev-parse syntax),
// walking commit history, and describing commits like `git describe`.
// We build a small scratch repository so the describe output is deterministic.

// ---------------------------------------------------------------------------
// 0. Scratch repository: three commits, an annotated tag on the first one
// ---------------------------------------------------------------------------
const base = mkdtempSync(join(tmpdir(), "revision-demo-"));
const repoPath = join(base, "scratch");
execSync(`git init --quiet --initial-branch=main ${JSON.stringify(repoPath)}`, {
  stdio: "ignore",
});
const git = (args: string) => execSync(`git ${args}`, { cwd: repoPath, stdio: "ignore" });
git("config user.email revision@example.com");
git("config user.name Revision Example");

const commit = (file: string, message: string) => {
  writeFileSync(join(repoPath, file), `${file}\n`);
  git(`add ${file}`);
  git(`commit --quiet -m ${JSON.stringify(message)}`);
};

commit("seed.txt", "seed");
git("tag -a v1.0.0 -m v1.0.0");
commit("second.txt", "second");
commit("third.txt", "third");

const repo = Repository.open(repoPath);
const head = repo.headId();

// ---------------------------------------------------------------------------
// 1. Resolving specs to ids
// ---------------------------------------------------------------------------
console.log("=== revParseSingle / revParseHex ===");
console.log(`HEAD        -> ${repo.revParseSingle("HEAD")}`);
console.log(`HEAD~2      -> ${repo.revParseSingle("HEAD~2")}`);
console.log(`v1.0.0      -> ${repo.revParseSingle("v1.0.0").slice(0, 8)}…`);
console.log(`v1.0.0^{commit} (peeled) -> ${repo.revParseHex("v1.0.0^{commit}").slice(0, 8)}…`);
console.log(`hash == HEAD: ${repo.revParseHex("HEAD") === head}`);

// ---------------------------------------------------------------------------
// 2. Spec structure: kinds, ranges, merges, exclusions
// ---------------------------------------------------------------------------
console.log("\n=== revParse ===");
const single = repo.revParse("HEAD");
console.log(
  `HEAD: kind=${single.kind} id=${single.id?.slice(0, 8)}… from=${single.from} to=${single.to}`,
);

const range = repo.revParse("HEAD~1..HEAD");
console.log(
  `HEAD~1..HEAD: kind=${range.kind} from=${range.from?.slice(0, 8)}… to=${range.to?.slice(0, 8)}…`,
);

const merge = repo.revParse("main...HEAD~1");
console.log(
  `main...HEAD~1: kind=${merge.kind} from=${merge.from?.slice(0, 8)}… to=${merge.to?.slice(0, 8)}…`,
);

const exclude = repo.revParse("^HEAD~1");
console.log(`^HEAD~1: kind=${exclude.kind} id=${exclude.id?.slice(0, 8)}…`);

const parents = repo.revParse("HEAD^@");
console.log(`HEAD^@: kind=${parents.kind} id=${parents.id?.slice(0, 8)}…`);

// ---------------------------------------------------------------------------
// 3. Walking history
// ---------------------------------------------------------------------------
console.log("\n=== revWalk ===");
const seedCommit = repo.revParseHex("v1.0.0^{commit}"); // peel the tag to its commit
const defaultWalk = repo.revWalk([head]);
console.log(
  `breadth-first: ${defaultWalk.length} commit(s), tip first: ${defaultWalk[0] === head}`,
);

const chronological = repo.revWalk([head], { sort: "time-oldest" });
console.log(
  `time-oldest: oldest is the seed commit: ${
    chronological[chronological.length - 1] === seedCommit
  }`,
);

const byTime = repo.revWalk([head], { sort: "time-newest" });
console.log(`time-newest: newest first: ${byTime[0] === head}`);

console.log(`firstParent: ${repo.revWalk([head], { firstParent: true }).length} commit(s)`);
console.log(`reverse: ${repo.revWalk([head], { reverse: true })[0] === seedCommit}`);

// ---------------------------------------------------------------------------
// 4. Describing commits
// ---------------------------------------------------------------------------
console.log("\n=== describe ===");
const tagged = seedCommit;
const exact = repo.describe(tagged, { names: "allTags" });
console.log(
  `describe(tag): name=${exact?.name} depth=${exact?.depth} format=${exact?.format} exactMatch=${exact?.exactMatch}`,
);

const ahead = repo.describe(head, { names: "allTags" });
console.log(`describe(HEAD): name=${ahead?.name} depth=${ahead?.depth} format=${ahead?.format}`);

// Candidate selection: branches count too when allRefs is used.
const viaBranch = repo.describe(head, { names: "allRefs" });
console.log(`describe(HEAD, allRefs): name=${viaBranch?.name} exactMatch=${viaBranch?.exactMatch}`);

// Deterministic fallback paths: with maxCandidates 0 nothing can be named.
const fallback = repo.describe(head, { maxCandidates: 0, idAsFallback: true });
console.log(`fallback: name=${fallback?.name} format=${fallback?.format} depth=${fallback?.depth}`);
console.log(`no fallback (null): ${repo.describe(head, { maxCandidates: 0 }) === null}`);

rmSync(base, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
console.log("\nrevision example complete.");
