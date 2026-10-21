import { Repository } from "../dist/index.js";

async function main() {
  const repo = Repository.open(process.cwd());

  console.log("=== Wave 1 Demo ===");
  console.log("HEAD tree ID:", repo.headTreeId());
  console.log("HEAD Ref:", repo.headRef()?.name);
  console.log("HEAD Tree entries count:", repo.headTree().entries.length);

  const remote = repo.remoteAt("https://github.com/example/repo.git");
  console.log("Remote at URL:", remote.url);

  console.log("Config entries count:", repo.configEntries().length);
  console.log("Describe HEAD:", repo.describe("HEAD"));
  console.log("Is shallow:", repo.isShallow());

  console.log("\n=== Wave 2 Demo ===");
  console.log("Index entries count:", repo.index().length);
  console.log("Index or load entries count:", repo.indexOrLoad().length);
  const status = await repo.status();
  console.log("Status entries count:", status.length);
  console.log("Index to worktree changes:", repo.diffIndexToWorktree().length);
  console.log("HEAD to index changes:", repo.diffHeadToIndex().length);

  console.log("\n=== Wave 3 Demo ===");
  console.log("Config bool core.bare:", repo.configBool("core.bare"));
  repo.setConfigValue("demo.key", "demovalue");
  console.log("Read set config value:", repo.configString("demo.key"));
  repo.unsetConfigValue("demo.key");
  console.log("After unset config value:", repo.configString("demo.key"));
  const branch = repo.headName()?.replace("refs/heads/", "") ?? "main";
  console.log(`Branch '${branch}' remote:`, repo.branchRemoteName(branch));
  console.log(`Branch '${branch}' tracking:`, repo.branchTrackingRef(branch));

  console.log("\n=== Wave 4 Demo ===");
  const blobId = repo.writeBlob(Buffer.from("wave 4 demo blob\n"));
  const treeId = repo.writeTree([{ name: "demo.txt", mode: "Blob", id: blobId }]);
  const commitId = repo.writeCommit(treeId, [], null, null, "Wave 4 demo commit");
  console.log("Written demo commit ID:", commitId);
  console.log("Try find object (commit):", repo.tryFindObject(commitId)?.kind);
  console.log("Rev parse hex HEAD:", repo.revParseHex("HEAD"));

  console.log("\n=== Merge, Attributes, State & Location Demo ===");
  console.log("Git dir trust:", repo.gitDirTrust());
  console.log("Is pristine:", repo.isPristine());
  console.log("Workdir path for Cargo.toml:", repo.workdirPath("Cargo.toml"));
  console.log("Attributes for Cargo.toml:", repo.attributes("Cargo.toml").length);
}

main().catch(console.error);
