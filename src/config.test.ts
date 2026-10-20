import { execSync } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { ConfigSource, Repository } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-config-"));
  cloneDir = join(base, "clone");
  try {
    repo = await Repository.clone(realRoot, cloneDir, false);
  } catch {
    rmSync(cloneDir, { recursive: true, force: true });
    execSync(`git clone --local --quiet ${JSON.stringify(realRoot)} ${JSON.stringify(cloneDir)}`, {
      stdio: "ignore",
    });
    repo = Repository.open(cloneDir);
  }
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true });
});

describe("Repository config (src/config.rs)", () => {
  // Values written here so the snapshot assertions don't depend on the host machine's git config.
  // NOTE: a `Repository` resolves configuration when it is opened, so values written via
  // `configSet()` are read back through a freshly opened instance (see the existing
  // `configSet()` test above, which does the same).
  const snapName = "core.snapName";
  const snapFlag = "core.snapFlag";
  const snapCount = "core.snapCount";

  beforeAll(() => {
    repo.configSet(snapName, "gitoxide");
    repo.configSet(snapFlag, "true");
    repo.configSet(snapCount, "7");
    // Observe the writes through a fresh instance.
    repo = Repository.open(cloneDir);
  });

  test("configString returns strings or null (never undefined)", () => {
    const signing = repo.configString("user.signingKey");
    expect(typeof signing === "string" || signing === null).toBe(true);
    expect(repo.configString("this.key.does.not.exist.zzz")).toBeNull();
  });

  test("configSet() sets a key that can be read via configString()", () => {
    repo.configSet("core.customSetting", "custom_value");
    expect(Repository.open(cloneDir).configString("core.customSetting")).toBe("custom_value");
  });

  test("configPath() resolves the expected file per source", () => {
    expect(repo.configPath(ConfigSource.Local)).toBe(join(repo.gitDir(), "config"));
    expect(repo.configPath(ConfigSource.Worktree)).toBe(join(repo.gitDir(), "config.worktree"));
    // the local config file exists in the sandbox clone
    expect(existsSync(repo.configPath(ConfigSource.Local))).toBe(true);
  });

  test("looseCompression() / packCompression() report levels 0-9", () => {
    const loose = repo.looseCompression();
    expect(Number.isInteger(loose)).toBe(true);
    expect(loose).toBeGreaterThanOrEqual(0);
    expect(loose).toBeLessThanOrEqual(9);

    const pack = repo.packCompression();
    expect(Number.isInteger(pack)).toBe(true);
    expect(pack).toBeGreaterThanOrEqual(0);
    expect(pack).toBeLessThanOrEqual(9);
  });

  test("bigFileThreshold() reports the configured byte threshold", () => {
    const t = repo.bigFileThreshold();
    expect(t).toBeGreaterThan(0);
    // git's default core.bigFileThreshold is 512MiB
    expect(t).toBe(536870912);
  });

  test("diffAlgorithm() reports Histogram or Myers", () => {
    const algo = repo.diffAlgorithm();
    expect(["Histogram", "Myers"]).toContain(algo);
  });

  test("configSnapshot() reads string/bool/int values with defaults", () => {
    const snap = repo.configSnapshot();
    expect(snap).not.toBeNull();

    expect(snap!.string(snapName)).toBe("gitoxide");
    expect(snap!.string("core.snapMissing")).toBe(null);

    expect(snap!.boolean(snapFlag, false)).toBe(true);
    // absent key falls back to the provided default
    expect(snap!.boolean("core.snapMissing", true)).toBe(true);
    // absent key with no default defaults to false
    expect(snap!.boolean("core.snapMissing")).toBe(false);

    expect(snap!.integer(snapCount, 0)).toBe(7);
    expect(snap!.integer("core.snapMissing", 42)).toBe(42);
    expect(snap!.integer("core.snapMissing")).toBe(0);
  });

  test("configSnapshot().strings() returns multivars or null", () => {
    expect(repo.configSnapshot()!.strings(snapName)).toEqual(["gitoxide"]);
    expect(repo.configSnapshot()!.strings("core.snapMissing")).toBe(null);
  });

  test("configSnapshot().entries() flattens the resolved config", () => {
    const snap = repo.configSnapshot()!;
    const entries = snap.entries();
    expect(Array.isArray(entries)).toBe(true);
    expect(entries.length).toBeGreaterThan(0);
    // Section names are flattened as `section.subsection.key`.
    expect(entries.some((e) => e.key === snapName && e.value === "gitoxide")).toBe(true);
  });
});
