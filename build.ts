import { NapiCli } from "@napi-rs/cli";

async function run() {
  const args = process.argv.slice(2);
  const isRelease = args.includes("--release") || args.includes("-r");
  const useNapiCross = args.includes("--use-napi-cross");
  const crossCompile = args.includes("--cross-compile") || args.includes("-x");
  const useCross = args.includes("--use-cross");

  const targetIdx = args.findIndex((a) => a === "--target" || a === "-t");
  const target = targetIdx !== -1 && args[targetIdx + 1] ? args[targetIdx + 1] : undefined;

  const cli = new NapiCli();
  // NOTE: @napi-rs/cli does `if (options.features) args.push("--features", ...)`, and `[]` is
  // truthy in JS — so an empty array emits a bare `--features` and cargo errors. Omit the key
  // entirely to build with default features.
  await cli.build({
    platform: true,
    esm: true,
    outputDir: "./dist",
    release: isRelease,
    target,
    useNapiCross,
    crossCompile,
    useCross,
    cargoOptions: ["--ignore-rust-version"],
  });
}

void run().catch((err) => {
  console.error(err);
  process.exit(1);
});
