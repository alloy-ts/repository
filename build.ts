import fs from "node:fs";
import { NapiCli } from "@napi-rs/cli";

function ensureCargoWrapper() {
  const wrapperPath = "/tmp/cargo-wrapper";
  if (!fs.existsSync(wrapperPath)) {
    const script = `#!/bin/bash
if [ "$1" = "build" ]; then
  shift
  exec cargo build --ignore-rust-version "$@"
else
  exec cargo "$@"
fi
`;
    fs.writeFileSync(wrapperPath, script, { mode: 0o755 });
  }
  process.env.CARGO = wrapperPath;
}

async function run() {
  ensureCargoWrapper();
  const args = process.argv.slice(2);
  const isRelease = args.includes("--release") || args.includes("-r");
  const useNapiCross = args.includes("--use-napi-cross");
  const crossCompile = args.includes("--cross-compile") || args.includes("-x");
  const useCross = args.includes("--use-cross");

  const targetIdx = args.findIndex((a) => a === "--target" || a === "-t");
  const target = targetIdx !== -1 && args[targetIdx + 1] ? args[targetIdx + 1] : undefined;

  const cli = new NapiCli();
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
}

void run().catch((err) => {
  console.error(err);
  process.exit(1);
});
