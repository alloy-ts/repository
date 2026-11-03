/**
 * Example demonstrating Bun-compatible Shell ($) and child_process APIs.
 * Run with: npx tsx examples/shell.ts
 */

import { $, exec, spawn } from "../src/main.ts";

async function main() {
  console.log("=== 1. Basic Echo and Text Output ===");
  const welcome = await $`echo "Hello from Bun-compatible shell with fusevm!"`.text();
  console.log(welcome.trim());

  console.log("\n=== 2. Builtin Utilities ===");
  const cwd = await $`pwd`.text();
  console.log("Current working directory:", cwd.trim());

  const dirname = await $`dirname /path/to/some/script.sh`.text();
  const basename = await $`basename /path/to/some/script.sh .sh`.text();
  console.log(`dirname: ${dirname.trim()}, basename: ${basename.trim()}`);

  console.log("\n=== 3. Pipeline & Lines Iterator ===");
  console.log("Printing numbers 1 to 3 via seq and lines iterator:");
  for await (const line of $`seq 1 3`.lines()) {
    console.log(` - ${line}`);
  }

  console.log("\n=== 4. JSON Output Handling ===");
  const packageJson = await $`echo '{"name": "@alloy-ts/repository", "fusevm": true}'`.json();
  console.log("Parsed JSON object:", packageJson);

  console.log("\n=== 5. Native Git/Repo Builtin ===");
  const isRepo = await $`git is-repo`.text();
  console.log("Is current directory a git repository?:", isRepo.trim());

  const repoStatus = await $`git status`.text();
  console.log("Repository status info:\n" + repoStatus.trim());

  console.log("\n=== 6. node:child_process Alias APIs ===");
  const execResult = await exec("echo 'exec child process output'");
  console.log("exec:", execResult.stdout.toString().trim());

  const spawnResult = await spawn("echo", ["spawn child process output"]);
  console.log("spawn:", spawnResult.stdout.toString().trim());

  console.log("\n=== Shell examples completed successfully! ===");
}

main().catch(console.error);
