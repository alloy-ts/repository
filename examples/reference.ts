import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  initBackend,
  refBranchNameIsValid,
  refCategoryFullName,
  type Reference,
  refNameIsValid,
  refNameSanitize,
  refPartialNameIsValid,
  RefStore,
  refStoreTypeName,
  Repository,
} from "../src/main.ts";

// References are the named pointers Git keeps in `refs/` (branches, tags,
// remote-tracking branches, notes) plus the special `HEAD`. This example walks
// the whole reference API against a throwaway repository, so it is safe to
// create, rename, and delete refs as it goes.

const base = mkdtempSync(join(tmpdir(), "reference-example-"));
const scratchRepoPath = join(base, "scratch-repo");
// `git init` creates the directory itself; the other commands need it to exist.
execSync(`git init --quiet --initial-branch=main ${JSON.stringify(scratchRepoPath)}`, {
  stdio: "ignore",
});
const git = (args: string) => execSync(`git ${args}`, { cwd: scratchRepoPath, stdio: "ignore" });
git("config user.email refs@example.com");
git("config user.name Reference Example");
// The throwaway repo inherits the ambient git config; signing is irrelevant here
// and would fail in environments without a usable key.
git("config commit.gpgsign false");
git("config tag.gpgsign false");
writeFileSync(join(scratchRepoPath, "README.md"), "# reference example\n");
git("add README.md");
git("commit --quiet -m 'first commit'");
writeFileSync(join(scratchRepoPath, "docs.md"), "docs\n");
git("add docs.md");
git("commit --quiet -m 'second commit'");

const repo = Repository.open(scratchRepoPath);

// ---------------------------------------------------------------------------
// 1. Reference names: validation, sanitization, and category round-trips
// ---------------------------------------------------------------------------
// These helpers are stateless -- no repository is needed.
console.log("=== 1. Reference names ===");
console.log(`refNameIsValid("refs/heads/main")       = ${refNameIsValid("refs/heads/main")}`);
console.log(`refNameIsValid("main")                  = ${refNameIsValid("main")}`);
console.log(`refNameIsValid("refs/heads/ma..in")     = ${refNameIsValid("refs/heads/ma..in")}`);
console.log(`refPartialNameIsValid("heads/main")     = ${refPartialNameIsValid("heads/main")}`);
console.log(`refPartialNameIsValid("x@{y}")          = ${refPartialNameIsValid("x@{y}")}`);
console.log(`refBranchNameIsValid("refs/heads/main") = ${refBranchNameIsValid("refs/heads/main")}`);
console.log(`refBranchNameIsValid("refs/heads/HEAD") = ${refBranchNameIsValid("refs/heads/HEAD")}`);
console.log(`refNameSanitize("feature//a b..c")      = ${refNameSanitize("feature//a b..c")}`);
console.log(`refNameSanitize("")                     = ${refNameSanitize("")}`);
console.log(
  `refCategoryFullName("remote_branch", "origin/main") = ${refCategoryFullName(
    "remote_branch",
    "origin/main",
  )}`,
);

// ---------------------------------------------------------------------------
// 2. HEAD: symbolic, detached, and unborn
// ---------------------------------------------------------------------------
console.log("\n=== 2. HEAD ===");
const head = repo.head();
console.log(`head():      referent=${head.referent ?? null} kind=${head.kind}`);
console.log(`headId():    ${repo.headId().slice(0, 8)}`);
console.log(`headName():  ${repo.headName()}`);
console.log(`headRef():   ${repo.headRef()?.name}`);
const headCommit = repo.headCommit();
console.log(`headCommit(): ${headCommit.id.slice(0, 8)} "${headCommit.message.split("\n")[0]}"`);
console.log(`headTreeId(): ${repo.headTreeId().slice(0, 8)}`);

// `kind` tells the three HEAD states apart without guessing from `referent`.
// setHead() takes an object id and detaches HEAD; there is no ref-name form.
repo.setHead(repo.headId());
console.log(
  `setHead(<oid>)             -> kind=${repo.head().kind} referent=${repo.head().referent ?? null}`,
);
// Re-attaching HEAD to a branch is a symbolic reference edit.
repo.editReferences([
  {
    name: "HEAD",
    newTarget: "refs/heads/main",
    force: true,
  },
]);
console.log(
  `symbolic HEAD edit         -> kind=${repo.head().kind} referent=${repo.head().referent ?? null}`,
);

const unbornDir = join(base, "unborn");
execSync(`git init --quiet --initial-branch=main ${JSON.stringify(unbornDir)}`, {
  stdio: "ignore",
});
const unbornHead = Repository.open(unbornDir).head();
console.log(
  `fresh \`git init\` repo      -> kind=${unbornHead.kind} referent=${unbornHead.referent ?? null}`,
);

// ---------------------------------------------------------------------------
// 3. Finding references
// ---------------------------------------------------------------------------
console.log("\n=== 3. Finding references ===");
const main = repo.findReference("refs/heads/main");
console.log(`findReference("refs/heads/main") -> ${main.name} = ${main.target.slice(0, 8)}`);
console.log(
  `tryFindReference("refs/heads/nope") -> ${
    repo.tryFindReference("refs/heads/nope")?.name ?? null
  }`,
);
console.log(`getReference("HEAD") (resolves)   -> ${repo.getReference("HEAD").slice(0, 8)}`);
try {
  repo.findReference("refs/heads/nope");
} catch (e) {
  console.log(`findReference("refs/heads/nope") throws: ${(e as Error).message.split("\n")[0]}`);
}

// ---------------------------------------------------------------------------
// 4. Inspecting a reference
// ---------------------------------------------------------------------------
console.log("\n=== 4. Inspecting a reference ===");
const headRef = repo.findReference("HEAD");
console.log(
  `HEAD  kind=${headRef.kind} symbolicTarget=${headRef.symbolicTarget} target="${headRef.target}"`,
);
console.log(`main  kind=${main.kind} target=${main.target.slice(0, 8)}`);
console.log(`  shorten()             = ${main.shorten()}`);
console.log(`  fileName()            = ${main.fileName()}`);
console.log(`  category()            = ${main.category()}`);
console.log(`  categoryAndShortName()= ${JSON.stringify(main.categoryAndShortName())}`);
// `targetPeel` is the packed-refs peel hint; it is only set for packed refs that
// point at a tag object, so it is `null` for loose refs like these.
console.log(`  targetPeel            = ${main.targetPeel}`);

// ---------------------------------------------------------------------------
// 5. Classifying references by name prefix
// ---------------------------------------------------------------------------
console.log("\n=== 5. Classifying references ===");
const tag = repo.createLightweightTag("v-lightweight", repo.headId(), true);
const note = repo.createReference("refs/notes/commits", repo.headId(), true);
const remote = repo.createReference("refs/remotes/origin/main", repo.headId(), true);
const rows: Array<[string, Reference]> = [
  ["HEAD", headRef],
  ["refs/heads/main", main],
  [tag.name, tag],
  [note.name, note],
  [remote.name, remote],
];
console.log("  name                        branch remote tag    note   pseudo");
for (const [label, r] of rows) {
  const flag = (v: boolean) => String(v).padEnd(6);
  console.log(
    `  ${label.padEnd(28)}${flag(r.isBranch())}${flag(r.isRemote())}${flag(r.isTag())}${flag(
      r.isNote(),
    )}${flag(r.isPseudoRef())}`,
  );
}

// ---------------------------------------------------------------------------
// 6. Resolving symbolic refs and peeling objects
// ---------------------------------------------------------------------------
console.log("\n=== 6. Resolving and peeling ===");
console.log(`HEAD.follow()            -> ${headRef.follow()?.name}   (one level)`);
console.log(`HEAD.resolve()           -> ${headRef.resolve().name}   (whole chain)`);
console.log(`HEAD.peelToId()          -> ${headRef.peelToId().slice(0, 8)}`);
console.log(`HEAD.peelToKind("commit")-> ${headRef.peelToKind("commit").id.slice(0, 8)}`);
console.log(`main.peelToCommit().id   -> ${main.peelToCommit().id.slice(0, 8)}`);
console.log(`main.peelToTree().id     -> ${main.peelToTree().id.slice(0, 8)}`);

const annotated = repo.createTag("v-annotated", repo.headId(), "annotated tag", true);
const annRef = repo.findReference(annotated.name);
console.log(`annotated tag ${annRef.name}:`);
console.log(`  target          = ${annRef.target.slice(0, 8)}  (the tag object)`);
console.log(`  peelToTag().id  = ${annRef.peelToTag().id.slice(0, 8)}`);
console.log(`  peelToCommit().id = ${annRef.peelToCommit().id.slice(0, 8)}  (== HEAD)`);
console.log(`  peelToId()      = ${annRef.peelToId().slice(0, 8)}  (end of the tag chain)`);

// ---------------------------------------------------------------------------
// 7. Listing references
// ---------------------------------------------------------------------------
console.log("\n=== 7. Listing references ===");
console.log(`references()             = ${repo.references().length} refs`);
console.log(
  `referencesPrefixed("refs/heads/") = ${repo
    .referencesPrefixed("refs/heads/")
    .map((r) => r.shorten())
    .join(", ")}`,
);
console.log(
  `referencesPseudo()       = ${repo
    .referencesPseudo()
    .map((r) => r.name)
    .join(", ")}`,
);
console.log(`referenceNames()         = ${repo.referenceNames().length} names`);
console.log(`referenceNames("refs/tags/") = ${repo.referenceNames("refs/tags/").join(", ")}`);
console.log(
  `referencesGlob("refs/tags/v-*") = ${repo
    .referencesGlob("refs/tags/v-*")
    .map((r) => r.name)
    .join(", ")}`,
);

// ---------------------------------------------------------------------------
// 8. Creating references
// ---------------------------------------------------------------------------
console.log("\n=== 8. Creating references ===");
const created = repo.createReference("refs/heads/example/create", repo.headId(), true);
console.log(`createReference()        -> ${created.name} = ${created.target.slice(0, 8)}`);
const raw = repo.tagReference("v-raw", repo.headId(), true);
console.log(`tagReference("v-raw")    -> ${raw.name} = ${raw.target.slice(0, 8)}`);

// A non-oid `newTarget` creates a symbolic reference.
repo.editReferences([
  {
    name: "refs/heads/example/symbolic",
    newTarget: "refs/heads/main",
    force: true,
  },
]);
const sym = repo.findReference("refs/heads/example/symbolic");
console.log(`editReferences(symbolic) -> ${sym.name} (${sym.kind}) -> ${sym.symbolicTarget}`);

// Batched edits are applied as one transaction and return the touched names.
const touched = repo.editReferences([
  { name: "refs/heads/example/batch-a", newTarget: repo.headId(), force: true },
  {
    name: "refs/heads/example/batch-b",
    newTarget: repo.headTreeId(),
    force: true,
  },
]);
console.log(`editReferences(batch)    -> ${JSON.stringify(touched)}`);

// ---------------------------------------------------------------------------
// 9. Mutating references
// ---------------------------------------------------------------------------
console.log("\n=== 9. Mutating references ===");
const mover = repo.createReference("refs/heads/example/mover", repo.headId(), true);
const moved = mover.setTarget(repo.headTreeId(), "example: point at the tree");
console.log(`setTarget(tree)          -> ${moved.name} = ${moved.target.slice(0, 8)}`);
// Every Reference is a snapshot: the original object still shows the old value,
// and reload() re-resolves by name.
console.log(`  snapshot still shows     ${mover.target.slice(0, 8)}`);
console.log(`  reload() shows           ${mover.reload()?.target.slice(0, 8)}`);

// Passing `expected` turns the update into a compare-and-swap.
try {
  mover.setTarget(repo.headId(), "example: cas", repo.headId());
  console.log("  cas with a stale expected value unexpectedly succeeded");
} catch {
  console.log("  cas with a stale expected value rejected");
}
const cas = mover.setTarget(repo.headId(), "example: cas", repo.headTreeId());
console.log(`  cas with the current value -> ${cas.target.slice(0, 8)}`);

const renamed = mover.rename("refs/heads/example/moved", false, "example: rename");
console.log(
  `rename()                 -> ${renamed.name} (old ref gone: ${
    repo.tryFindReference("refs/heads/example/mover") === null
  })`,
);
try {
  renamed.rename("refs/heads/main");
} catch {
  console.log("  rename() onto an existing ref rejected without force");
}

const repointed = sym.setSymbolicTarget("refs/heads/example/create", "example: repoint");
console.log(`setSymbolicTarget()      -> ${repointed.name} -> ${repointed.symbolicTarget}`);

renamed.delete();
console.log(
  `delete()                 -> tryFindReference = ${
    repo.tryFindReference(renamed.name)?.name ?? null
  }`,
);

// ---------------------------------------------------------------------------
// 10. Reflogs
// ---------------------------------------------------------------------------
console.log("\n=== 10. Reflogs ===");
const logged = repo.createReference("refs/heads/example/logged", repo.headId(), true);
logged.setTarget(repo.headTreeId(), "example: second value");
console.log(`reflogExists() = ${repo.reflogExists("refs/heads/example/logged")}`);
for (const entry of repo.reflog("refs/heads/example/logged")) {
  console.log(
    `  ${entry.previousOid.slice(0, 8)} -> ${entry.newOid.slice(
      0,
      8,
    )}  ${entry.signature.name} <${entry.signature.email}>  ${entry.message}`,
  );
}
console.log(
  `reflog(reverse) newest first = "${
    repo.reflog("refs/heads/example/logged", true)[0]?.message ?? "(none)"
  }"`,
);

// ---------------------------------------------------------------------------
// 11. Packed refs
// ---------------------------------------------------------------------------
console.log("\n=== 11. Packed refs ===");
console.log(`packedRefsPath() = ${repo.packedRefsPath()}`);
console.log(
  `setPackedBufferMmapThreshold(65536) -> previous ${repo.setPackedBufferMmapThreshold(65536)}`,
);
execSync("git pack-refs --all", { cwd: scratchRepoPath, stdio: "ignore" });
repo.forceRefreshPackedBuffer();
console.log(`after \`git pack-refs --all\`: ${repo.referenceNames().length} refs still readable`);
// Once packed, an annotated tag carries its peel target from packed-refs.
const packedAnn = repo.tryFindReference(annotated.name);
console.log(
  `packed annotated tag: target=${packedAnn?.target.slice(0, 8)} targetPeel=${
    packedAnn?.targetPeel?.slice(0, 8) ?? null
  }`,
);

// ---------------------------------------------------------------------------
// 12. RefStore: the low-level git_ref_store parity layer
// ---------------------------------------------------------------------------
console.log("\n=== 12. RefStore ===");
const store = RefStore.open(repo);
console.log(
  `storeType = ${refStoreTypeName(store.storeType)} (default = ${refStoreTypeName(initBackend())})`,
);
console.log(`gitDir = ${store.gitDir}`);
console.log(`exists("refs/heads/main") = ${store.exists("refs/heads/main")}`);
console.log(
  `lookup("HEAD")            = ${store.lookup("HEAD").name} -> ${
    store.lookup("HEAD").symbolicTarget
  }`,
);
console.log(`resolve("HEAD")           = ${store.resolve("HEAD").name}`);
console.log(`tryLookup("refs/heads/nope") = ${store.tryLookup("refs/heads/nope")?.name ?? null}`);

// The iterator walks refs lazily; nextName() skips materialising the objects.
const iter = store.iterator("refs/heads/example/*");
const iterNames: string[] = [];
for (let n = iter.nextName(); n !== null; n = iter.nextName()) {
  iterNames.push(n);
}
console.log(`iterator("refs/heads/example/*") = ${iterNames.join(", ")}`);
console.log(`listNames("refs/tags/*")  = ${store.listNames("refs/tags/*").join(", ")}`);

const written = store.write(
  "refs/heads/example/stored",
  repo.headId(),
  true,
  undefined,
  "example: refstore write",
);
console.log(`write()  -> ${written.name} = ${written.target.slice(0, 8)}`);
console.log(
  `rename() -> ${store.rename("refs/heads/example/stored", "refs/heads/example/renamed").name}`,
);
store.delete("refs/heads/example/renamed");
console.log(`delete() -> exists = ${store.exists("refs/heads/example/renamed")}`);

// lock()/unlock() mirror git_ref_store_lock/unlock: the write is deferred to unlock.
const lock = store.lock("refs/heads/main");
console.log(`lock("refs/heads/main") acquired for ${lock.refName}`);
// The write is deferred to unlock(); here main is moved back to the first commit.
const firstCommit = repo.headCommit().parentIds[0] ?? repo.headId();
const unlocked = lock.unlock(firstCommit, true, undefined, "example: unlock");
console.log(`unlock() -> ${unlocked.name} = ${unlocked.target.slice(0, 8)}`);

console.log(`shouldWriteReflog("refs/heads/main") = ${store.shouldWriteReflog("refs/heads/main")}`);
console.log(
  `reflogRead("refs/heads/main") = ${store.reflogRead("refs/heads/main").length} entries`,
);

rmSync(base, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
console.log("\nreference example complete.");
