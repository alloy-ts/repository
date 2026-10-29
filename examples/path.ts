import { mkdirSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  pathIsAbsolute,
  pathNormalise,
  pathNormaliseAndClean,
  pathNormaliseSaturating,
  pathRealpath,
  pathRealpathWithCwd,
  pathRelativizeWithPrefix,
  pathToNativeSeparators,
  pathToUnixSeparators,
} from "../src/main.ts";

// Path utilities are pure string operations — no repository required. They mirror
// git's own handling: virtual `..` elimination without touching the filesystem,
// symlink resolution, cwd-relative display paths, and separator conversion.

// ---------------------------------------------------------------------------
// 1. realpath: symlink resolution
// ---------------------------------------------------------------------------
console.log("=== pathRealpath ===");
// Resolve symlinks on a real directory (macOS: /tmp -> private/tmp).
console.log(`pathRealpath("/tmp") -> ${pathRealpath("/tmp")}`);
console.log(`pathRealpath(".", 16) -> ${pathRealpath(".", 16)}`);

// Symlink resolution in a scratch dir, resolved against an explicit cwd.
const base = join(tmpdir(), "path-demo-");
rmSync(base, { recursive: true, force: true });
mkdirSync(join(base, "real"), { recursive: true });
writeFileSync(join(base, "real", "file.txt"), "hello\n");
symlinkSync("real", join(base, "link"));
console.log(
  `pathRealpathWithCwd("link/file.txt", ${base}) -> ${pathRealpathWithCwd("link/file.txt", base)}`,
);

// Error cases: missing components are kept as-is, but cycles throw.
console.log(
  `pathRealpathWithCwd("does/not/exist", ${base}) -> ${pathRealpathWithCwd(
    "does/not/exist",
    base,
  )}`,
);
symlinkSync("loop", join(base, "loop"));
try {
  pathRealpathWithCwd("loop", base, 4);
} catch (e) {
  console.log(`symlink cycle rejected: ${(e as Error).message.split("\n")[0]}`);
}
rmSync(base, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });

// ---------------------------------------------------------------------------
// 2. normalise: virtual `..` elimination without filesystem access
// ---------------------------------------------------------------------------
console.log("\n=== pathNormalise ===");
const cwd = "/cwd";
console.log(`normalise("a/./b/c/.././..") -> ${pathNormalise("a/./b/c/.././..", cwd)}`);
console.log(`normalise("/a/../b/..")       -> ${pathNormalise("/a/../b/..", cwd)}`);
console.log(`normalise("/base/a/..")       -> ${pathNormalise("/base/a/..", cwd)}`);
console.log(`normalise("./a/..")           -> ${pathNormalise("./a/..", cwd)}`);
// Leading dots and duplicate separators are preserved by design:
console.log(`normalise(".///")             -> ${pathNormalise(".///", cwd)}`);
console.log(`normalise("a//b")             -> ${pathNormalise("a//b", cwd)}`);
// Popping past the current dir is inconsistent -> null; saturating clamps instead:
console.log(`normalise("../../r") -> ${pathNormalise("../../r", "/c")}`);
console.log(`normaliseSaturating("../../r") -> ${pathNormaliseSaturating("../../r", "/c")}`);
// normaliseAndClean also strips dot components and duplicate/trailing separators:
console.log(`normaliseAndClean("a//b/.") -> ${pathNormaliseAndClean("a//b/.", cwd)}`);
console.log(`normaliseAndClean("a/../..") -> ${pathNormaliseAndClean("a/../..", cwd)}`);

// ---------------------------------------------------------------------------
// 3. Display paths: relativize, absolute check, separators
// ---------------------------------------------------------------------------
console.log("\n=== Display paths ===");
console.log(
  `relativizeWithPrefix("src/lib/main.rs", "src/lib") -> ${pathRelativizeWithPrefix(
    "src/lib/main.rs",
    "src/lib",
  )}`,
);
console.log(
  `relativizeWithPrefix("src/lib/main.rs", "docs")    -> ${pathRelativizeWithPrefix(
    "src/lib/main.rs",
    "docs",
  )}`,
);
// Empty prefix is a no-op.
console.log(
  `relativizeWithPrefix("src/lib/main.rs", "")        -> ${pathRelativizeWithPrefix(
    "src/lib/main.rs",
    "",
  )}`,
);

// Git's rule: absolute by platform, or anything that looks like a Unix path.
console.log(
  `isAbsolute: /tmp=${pathIsAbsolute("/tmp")} C:\\x=${pathIsAbsolute(
    "C:\\x",
  )} a/b=${pathIsAbsolute("a/b")}`,
);

console.log(`toUnixSeparators("a\\b\\c")   -> ${pathToUnixSeparators("a\\b\\c")}`);
console.log(`toNativeSeparators("a/b/c") -> ${pathToNativeSeparators("a/b/c")}`);

console.log("\npath example complete.");
