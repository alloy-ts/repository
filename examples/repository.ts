import { Repository } from "../dist/index.js";

console.log("--- Repository API Examples ---");

const repo = Repository.open(process.cwd());

console.log("HEAD ID:", repo.headId());
console.log("HEAD Tree ID:", repo.headTreeId());
console.log("HEAD Ref:", repo.headRef()?.name);
console.log("Is Shallow:", repo.isShallow());

const configEntries = repo.configEntries();
console.log(`Config Entries (${configEntries.length} found):`);
for (const entry of configEntries.slice(0, 5)) {
  console.log(`  ${entry.key} = ${entry.value}`);
}

const remote = repo.remoteAt("https://github.com/example/repo.git");
console.log("Anonymous Remote URL:", remote.url);

const indexEntries = repo.index();
console.log(`Index Entries (${indexEntries.length} found):`);
for (const entry of indexEntries.slice(0, 3)) {
  console.log(`  ${entry.path} (${entry.mode}, ${entry.id.slice(0, 7)})`);
}

const statusEntries = await repo.status();
console.log(`Status Entries (${statusEntries.length} found)`);

const branches = repo.branchNames();
console.log(`Local Branches (${branches.length} found):`, branches);

const blobId = repo.writeBlob(Buffer.from("Hello from gix wave 4\n"));
const treeId = repo.writeTree([{ name: "hello.txt", mode: "100644", id: blobId }]);
console.log("Written Tree ID:", treeId);

const worktrees = repo.worktrees();
console.log(`Worktrees (${worktrees.length} found):`, worktrees.map(w => w.path));

const headCommit = repo.headCommit();
const mergeOutcome = repo.mergeTrees(headCommit.treeId, headCommit.treeId, null);
console.log("Merge Trees Has Conflicts:", mergeOutcome.hasConflicts);

const isIgnored = repo.isPathIgnored("node_modules/test.js");
console.log("node_modules/test.js Is Ignored:", isIgnored);

console.log("Repository State:", repo.state());
console.log("Submodules Count:", repo.submodules().length);
