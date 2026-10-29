import { readFileSync } from "node:fs";
import { NapiCli } from "@napi-rs/cli";

async function run() {
  const args = process.argv.slice(2);
  const isRelease = args.includes("--release") || args.includes("-r");
  const useNapiCross = args.includes("--use-napi-cross");
  const crossCompile = args.includes("--cross-compile") || args.includes("-x");
  const useCross = args.includes("--use-cross");
  const silent = args.includes("--silent");
  const verbose = args.includes("--verbose");

  const targetIdx = args.findIndex((a) => a === "--target" || a === "-t");
  const target = targetIdx !== -1 && args[targetIdx + 1] ? args[targetIdx + 1] : undefined;

  const cli = new NapiCli();
  const startedAt = new Date().toISOString();

  // NOTE: @napi-rs/cli does `if (options.features) args.push("--features", ...)`, and `[]` is
  // truthy in JS — so an empty array emits a bare `--features` and cargo errors. Omit the key
  // entirely to build with default features.
  //
  // The `ssh`/`xmss` features are still on the pre-migration code (ring/sha2/ssh-key/pq-xmss) and
  // don't compile yet — Phase 2. Once migrated, restore:
  //   features: ["ssh", "xmss"],
  await cli.build({
    platform: true,
    esm: true,
    outputDir: "./dist",
    release: isRelease,
    target,
    useNapiCross,
    crossCompile,
    useCross,
  });

  if (silent) return;

  // The native module is now built, so the `Repository` binding exists. Resolve the
  // current commit sha via the public API (mirrors `git rev-parse HEAD`).
  const { Repository } = await import("./dist/index.js");
  const pkg = JSON.parse(readFileSync("package.json", "utf8")) as {
    version: string;
  };
  let sha = "unborn";
  try {
    const repo = new Repository();
    sha = repo.headCommitId ?? "unborn";
  } catch {
    sha = "unknown";
  }

  console.log(`Build time: ${startedAt}`);
  console.log(`Build version: ${pkg.version}+${sha}`);

  if (verbose) {
    console.log(`Node: ${process.version}`);
    console.log(`Platform: ${process.platform} ${process.arch}`);
    console.log(`Target: ${target ?? "host"}`);
    console.log(`Profile: ${isRelease ? "release" : "dev"}`);
    console.log(
      `Napi cross: ${useNapiCross}, cross-compile: ${crossCompile}, use-cross: ${useCross}`,
    );
  }
}

void run().catch((err) => {
  console.error(err);
  process.exit(1);
});
