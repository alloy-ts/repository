import { Repository } from "../dist/index.js";

const repo = Repository.open(process.cwd());

console.log("Wave 1 - HEAD Management:");
console.log("HEAD Tree ID:", repo.headTreeId());
console.log("HEAD Ref:", repo.headRef()?.name ?? "detached");
console.log("HEAD Tree entries count:", repo.headTree().entries.length);

console.log("\nWave 1 - Remote:");
const anonRemote = repo.remoteAt("https://github.com/example/repo.git");
console.log("Anonymous Remote URL:", anonRemote.url);

console.log("\nWave 1 - Config:");
console.log("Total Config Entries:", repo.configEntries().length);

console.log("\nWave 1 - Revision Helpers:");
console.log("HEAD Describe:", repo.describe("HEAD"));
console.log("Is Shallow Repo:", repo.isShallow());

console.log("\nWave 2 - Index & Status:");
console.log("Index Entry Count:", repo.indexOrLoad().length);
console.log("Working Tree Status Count:", repo.indexWorktree().length);

console.log("\nWave 3 - Config Editing & Branch Helpers:");
repo.editConfig("user.demoKey", "demoValue");
console.log("Read custom config key user.demoKey:", repo.configString("user.demoKey"));
console.log("Local Branch Names:", repo.branchNames());
