/**
 * Comprehensive, runnable tour of the `Repository` binding.
 *
 *   node --import @oxc-node/core/register examples/repository.ts
 *
 * Everything runs against a throwaway sandbox: the working repository is cloned into a temp
 * directory, so no ref, file or config in the real repository is ever modified. Every section is
 * wrapped in `run()` so a failure in one area reports and the rest still execute.
 */

import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Repository } from "../dist/index.js";

/** Run a labelled section, reporting (not throwing) on failure. */
function run(label: string, fn: () => void): void {
  try {
    fn();
  } catch (e) {
    console.log(`  ! ${label}: ${e instanceof Error ? e.message : String(e)}`);
  }
}

async function main(): Promise<void> {
  // --- sandbox ---------------------------------------------------------------
  const realRoot = process.cwd();
  const base = mkdtempSync(join(tmpdir(), "repo-example-"));
  const cloneDir = join(base, "clone");
  let repo: Repository;

  try {
    repo = await Repository.clone(realRoot, cloneDir, false);
  } catch {
    // Fallback: git CLI init + fetch (avoids the origin/HEAD ref collision).
    rmSync(cloneDir, { recursive: true, force: true });
    const branch = Repository.open(realRoot).headName() ?? "refs/heads/main";
    const git = (args: string) => execSync(`git ${args}`, { stdio: "ignore" });
    git(`init --quiet ${JSON.stringify(cloneDir)}`);
    git(`-C ${JSON.stringify(cloneDir)} remote add origin ${JSON.stringify(realRoot)}`);
    git(
      `-C ${JSON.stringify(cloneDir)} fetch --quiet origin "+refs/heads/*:refs/remotes/origin/*"`,
    );
    git(`-C ${JSON.stringify(cloneDir)} fetch --quiet origin "+${branch}:${branch}"`);
    git(`-C ${JSON.stringify(cloneDir)} symbolic-ref HEAD ${branch}`);
    repo = Repository.open(cloneDir);
  }

  // Ensure example sandbox has at least 2 commits so HEAD~1 is valid.
  if (repo.log().length < 2) {
    const head = repo.headCommit();
    repo.commit(repo.headName() ?? "HEAD", "second commit in example sandbox", head.treeId, [
      head.id,
    ]);
  }

  console.log(`sandbox: ${repo.gitDir()}`);

  // --- 1. construction -------------------------------------------------------
  console.log("\n1. construction");
  run("discover", () => console.log(`  discover(".") -> ${Repository.discover(".").gitDir()}`));
  run("init (bare)", () => {
    const bare = Repository.init(join(base, "bare.git"), true);
    console.log(`  init(bare) isBare=${bare.isBare()}`);
  });
  run("open", () => console.log(`  open -> ${Repository.open(realRoot).gitDir()}`));
  run("openWith", () => {
    const isolated = Repository.openWith(cloneDir, { permissions: "isolated" });
    console.log(`  openWith(isolated) head=${isolated.headId()}`);
  });
  run("initOpts", () => {
    const wt = Repository.initOpts(join(base, "init-opts-wt"), {});
    console.log(`  initOpts(non-bare) isBare=${wt.isBare()}`);
  });

  // --- 2. HEAD & objects -----------------------------------------------------
  console.log("\n2. head & objects");
  run("head", () => {
    console.log(`  headId=${repo.headId()}`);
    console.log(`  headName=${repo.headName()}`);
    console.log(`  headTreeId=${repo.headTreeId()}`);
    console.log(`  headCommit.subject=${repo.headCommit().message.split("\n")[0]}`);
  });
  run("find*", () => {
    const commit = repo.findCommit(repo.headId());
    console.log(`  findCommit tree=${commit.treeId} parents=${commit.parentIds.length}`);
    const tree = repo.findTree(commit.treeId);
    console.log(`  findTree entries=${tree.entries.length}`);
    console.log(`  findObject kind=${repo.findObject(repo.headId()).kind}`);
    console.log(`  objectExists=${repo.objectExists(repo.headId())}`);
    console.log(`  tryFindObject(missing)=${JSON.stringify(repo.tryFindObject("0".repeat(40)))}`);
  });

  // --- 3. object writing -----------------------------------------------------
  console.log("\n3. object writing");
  run("writeBlob / writeTree", () => {
    const blob = repo.writeBlob(Buffer.from("example blob\n"));
    const tree = repo.writeTree([{ path: "example.txt", mode: "100644", id: blob }]);
    console.log(`  writeBlob=${blob}`);
    console.log(`  writeTree=${tree}`);
  });
  run("writeCommit (loose, no ref move)", () => {
    const id = repo.writeCommit(repo.headTreeId(), [repo.headId()], "example commit\n");
    console.log(`  writeCommit=${id}`);
    console.log(`  HEAD still ${repo.revParseSingle("HEAD")}`);
  });
  run("writeTag + peel", () => {
    const tag = repo.writeTag(repo.headId(), "commit", "example-tag", "example tag\n");
    console.log(`  writeTag=${tag}`);
    console.log(`  peel(tag -> commit)=${repo.peel(tag, "commit")}`);
    console.log(`  peel(commit -> tree)=${repo.peel(repo.headId(), "tree")}`);
  });

  // --- 4. references ---------------------------------------------------------
  console.log("\n4. references");
  run("references", () => {
    const refs = repo.references();
    console.log(`  count=${refs.length} e.g. ${refs[0]?.name}`);
    console.log(`  findReference(HEAD)=${repo.findReference("HEAD").name}`);
  });
  run("createReference + reflog", () => {
    const name = "refs/heads/example-probe";
    repo.createReference(name, repo.headId());
    console.log(`  created ${name} -> ${repo.revParseSingle(name)}`);
    console.log(`  reflog entries=${repo.reflog(name).length}`);
  });
  run("tags", () => {
    const tag = repo.createLightweightTag("example-lightweight", repo.headId());
    console.log(`  createLightweightTag=${tag.name}`);
  });

  // --- 5. remotes ------------------------------------------------------------
  console.log("\n5. remotes");
  run("remotes", () => {
    console.log(
      `  remotes=${repo
        .remotes()
        .map((r) => r.name)
        .join(", ")}`,
    );
    const origin = repo.findRemote("origin");
    console.log(`  origin url=${origin.url}`);
    console.log(`  remoteAt=${repo.remoteAt("https://example.com/owner/repo.git").url}`);
  });

  // --- 6. config -------------------------------------------------------------
  console.log("\n6. config");
  run("read", () => {
    console.log(`  entries=${repo.configEntries().length}`);
    console.log(`  core.bare=${repo.configString("core.bare")}`);
    console.log(`  core.bare (bool)=${repo.configBoolean("core.bare")}`);
    console.log(`  branchNames=${JSON.stringify(repo.branchNames())}`);
  });
  run("write", () => {
    repo.setConfigValue("example.setting", "hello");
    console.log(`  after set = ${repo.configString("example.setting")}`);
    console.log(`  removed = ${repo.unsetConfig("example", undefined)}`);
  });

  // --- 7. index & status -----------------------------------------------------
  console.log("\n7. index & status");
  run("index", () => {
    console.log(`  index entries=${repo.index().length}`);
    console.log(`  indexOrLoad entries=${repo.indexOrLoad().length}`);
  });
  run("status (async)", async () => {
    writeFileSync(join(cloneDir, "example-untracked.txt"), "untracked\n");
    const changes = await repo.status();
    console.log(`  changes=${changes.length}`);
    const untracked = changes.find((c) => c.kind === "untracked");
    if (untracked) console.log(`  e.g. ${untracked.path} (${untracked.kind})`);
  });
  run("editIndex (stage + unstage)", () => {
    const blob = repo.writeBlob(Buffer.from("staged via editIndex\n"));
    const path = "staged-example.txt";
    repo.editIndex([{ path, id: blob, mode: "100644" }]);
    const staged = Repository.open(cloneDir)
      .index()
      .find((e) => e.path === path);
    console.log(`  staged=${staged?.id === blob} stage=${staged?.stage}`);
    repo.editIndex(undefined, [path]);
    console.log(
      `  removed=${
        Repository.open(cloneDir)
          .index()
          .find((e) => e.path === path) === undefined
      }`,
    );
  });
  run("isDirty", () => console.log(`  isDirty=${repo.isDirty()}`));

  // --- 8. revisions & log ----------------------------------------------------
  console.log("\n8. revisions & log");
  run("revParse / revWalk / mergeBase", () => {
    const head = repo.revParseSingle("HEAD");
    console.log(`  HEAD=${head}`);
    console.log(`  HEAD~1=${repo.revParseSingle("HEAD~1")}`);
    console.log(`  revWalk first 3=${repo.revWalk([head]).slice(0, 3).join(", ")}`);
    console.log(`  mergeBase=${repo.mergeBase(head, repo.revParseSingle("HEAD~1"))}`);
    console.log(`  isAncestor=${repo.isAncestor(repo.revParseSingle("HEAD~1"), head)}`);
  });
  run("describe", () => console.log(`  describe=${repo.describe("HEAD")}`));
  run("log", () => {
    const entries = repo.log("HEAD", 3, false);
    console.log(`  log(3)=${entries.map((e) => e.subject).join(" | ")}`);
  });

  // --- 9. diff ---------------------------------------------------------------
  console.log("\n9. diff");
  run("diffTreeToTree", () => {
    const headTree = repo.headTreeId();
    const parentTree = repo.findCommit(repo.revParseSingle("HEAD~1")).treeId;
    const changes = repo.diffTreeToTree(parentTree, headTree);
    console.log(`  changes=${changes.length}`);
    if (changes[0]) console.log(`  e.g. ${changes[0].path} (${changes[0].kind})`);
  });

  // --- 10. worktrees ---------------------------------------------------------
  console.log("\n10. worktrees");
  run("worktrees", () => {
    console.log(`  linked worktrees=${repo.worktrees().length}`);
    console.log(`  isWorktree=${repo.isWorktree()}`);
    console.log(`  mainRepoPath=${repo.mainRepoPath()}`);
  });

  // --- 11. merge & blame -----------------------------------------------------
  console.log("\n11. merge & blame");
  run("mergeTrees", () => {
    const head = repo.findCommit(repo.headId()).treeId;
    const out = repo.mergeTrees(head, head, head);
    console.log(`  mergeTrees tree=${out.treeId} conflicts=${out.conflicts}`);
  });
  run("blameFile", () => {
    const tree = repo.headTree();
    const first = tree.entries.find((e) => e.mode === "Blob");
    if (first) {
      const hunks = repo.blameFile(first.name);
      console.log(`  blame ${first.name} hunks=${hunks.length}`);
    }
  });

  // --- 12. attributes / excludes / submodules --------------------------------
  console.log("\n12. attributes / excludes / submodules");
  run("attributes", () => {
    const tree = repo.headTree();
    const first = tree.entries.find((e) => e.mode === "Blob");
    if (first) {
      const attrs = repo.attributes(first.name);
      console.log(`  attributes for ${first.name}=${attrs.length}`);
      console.log(`  isPathExcluded=${repo.isPathExcluded(first.name)}`);
    }
  });
  run("submodules", () => {
    console.log(`  submodules=${repo.submodules().length}`);
    console.log(`  modulesPath=${repo.modulesPath()}`);
  });

  // --- 13. identity & state --------------------------------------------------
  console.log("\n13. identity & state");
  run("identity", () => {
    console.log(`  author=${repo.authorIdentity()}`);
    console.log(`  committer=${repo.committerIdentity()}`);
    console.log(`  defaultAuthor=${repo.defaultAuthor()?.name}`);
  });
  run("state", () => console.log(`  stateInProgress=${repo.stateInProgress()}`));

  // --- 14. paths & flags -----------------------------------------------------
  console.log("\n14. paths & flags");
  run("paths", () => {
    console.log(`  gitDir=${repo.gitDir()}`);
    console.log(`  workDir=${repo.workDir()}`);
    console.log(`  commonDir=${repo.commonDir()}`);
    console.log(`  isBare=${repo.isBare()} kind=${repo.kind()} objectHash=${repo.objectHash()}`);
    console.log(`  isShallow=${repo.isShallow()} shallowCommits=${repo.shallowCommits().length}`);
  });

  rmSync(base, { recursive: true, force: true });
  console.log("\ndone");
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
