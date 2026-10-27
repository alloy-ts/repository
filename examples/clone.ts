import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Cloner, Repository } from "../src/main.ts";

// This example clones a throwaway repository that lives entirely on local disk, so
// it needs no network. We build a bare "origin", push a commit to it, then use both
// `Repository.clone` and the `Cloner` class to clone it back out.

const base = mkdtempSync(join(tmpdir(), "clone-example-"));
const originPath = join(base, "origin.git");
execSync(`git init --bare --quiet ${JSON.stringify(originPath)}`);

const seedPath = mkdtempSync(join(tmpdir(), "clone-seed-"));
execSync(`git init --quiet --initial-branch=main ${JSON.stringify(seedPath)}`, {
  stdio: "ignore",
});
const git = (args: string, cwd = seedPath) => execSync(`git ${args}`, { cwd, stdio: "ignore" });
git("config user.email clone@example.com");
git("config user.name Clone Example");
git("config commit.gpgsign false");
writeFileSync(join(seedPath, "README.md"), "# clone example\n");
git("add README.md");
git("commit --quiet -m 'first commit'");
git(`remote add origin ${JSON.stringify(originPath)}`);
git("push -u origin HEAD");

// Auto-derived destinations resolve against the current working directory, so
// switch into `base` to keep every artifact under the temp dir.
process.chdir(base);

// ---------------------------------------------------------------------------
// 1. Pure URL helpers (no clone needed)
// ---------------------------------------------------------------------------
console.log("=== 1. URL helpers ===");
console.log(
  `isValidGitUrl("https://github.com/u/r.git") = ${Cloner.isValidGitUrl(
    "https://github.com/u/r.git",
  )}`,
);
console.log(`isValidGitUrl("not-a-url")                  = ${Cloner.isValidGitUrl("not-a-url")}`);
console.log(
  `extractRepoName("git@github.com:u/r.git")    = ${Cloner.extractRepoName(
    "git@github.com:u/r.git",
  )}`,
);
console.log(
  `convertSshToHttps("git@github.com:u/r.git") = ${Cloner.convertSshToHttps(
    "git@github.com:u/r.git",
  )}`,
);
console.log(
  `convertSshToHttps("https://x/y/r.git")      = ${Cloner.convertSshToHttps("https://x/y/r.git")}`,
);
console.log(
  `Repository.isValidGitUrl("https://x/y/r.git") = ${Repository.isValidGitUrl(
    "https://x/y/r.git",
  )}`,
);

// ---------------------------------------------------------------------------
// 2. Repository.clone with an explicit destination
// ---------------------------------------------------------------------------
console.log("\n=== 2. Repository.clone (explicit path) ===");
const repo = await Repository.clone(originPath, join(base, "explicit"));
console.log(`cloned into ./explicit -> headId ${repo.headId().slice(0, 8)}`);

// ---------------------------------------------------------------------------
// 3. Repository.clone without a path (auto-derived destination name)
// ---------------------------------------------------------------------------
console.log("\n=== 3. Repository.clone (auto-derived path) ===");
// `origin.git` derives to "origin"; it is created next to the bare repo (here, in `base`).
const repo2 = await Repository.clone(originPath);
console.log(`auto-cloned into ./origin -> headId ${repo2.headId().slice(0, 8)}`);
// The lenient auto-derive also works for bare local paths, unlike the strict
// `extractRepoName` helper used above.
rmSync(join(base, "origin"), { recursive: true, force: true });

// ---------------------------------------------------------------------------
// 4. The Cloner class: a reusable authenticator + progress callback
// ---------------------------------------------------------------------------
console.log("\n=== 4. Cloner (explicit path + progress) ===");
const cloner = new Cloner(); // no auth needed for a local clone
const repo3 = await cloner.clone(originPath, join(base, "via-cloner"), {
  progress: (e) => {
    if (e.percent !== undefined) {
      console.log(`  ${e.name ?? "?"} ${e.percent}%`);
    }
  },
});
console.log(`cloned via Cloner -> headId ${repo3.headId().slice(0, 8)}`);

// ---------------------------------------------------------------------------
// 5. Cloner.clone without a path also auto-derives the destination
// ---------------------------------------------------------------------------
console.log("\n=== 5. Cloner (auto-derived path) ===");
const repo4 = await cloner.clone(originPath); // reuses "origin" (removed above)
console.log(`auto-cloned via Cloner -> headId ${repo4.headId().slice(0, 8)}`);

process.chdir(tmpdir());
rmSync(base, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
console.log("\nclone example complete.");
