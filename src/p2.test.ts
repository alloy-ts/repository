import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { ConfigSource, Repository } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

function git(args: string, cwd: string) {
  execSync(`git ${args}`, { cwd, stdio: "ignore" });
}

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-p2-"));
  cloneDir = join(base, "clone");
  try {
    repo = await Repository.clone(realRoot, cloneDir, false);
  } catch (e) {
    rmSync(cloneDir, { recursive: true, force: true });
    execSync(`git clone --local --quiet ${JSON.stringify(realRoot)} ${JSON.stringify(cloneDir)}`, {
      stdio: "ignore",
    });
    repo = Repository.open(cloneDir);
  }
  writeFileSync(join(cloneDir, "p2_fixture.txt"), "p2 fixture content\n");
  git("add -A", cloneDir);
  git("config commit.gpgsign false", cloneDir);
  git('-c user.email=test@example.com -c user.name=Test commit -m "p2 fixture commit"', cloneDir);
  repo = Repository.open(cloneDir);
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true });
});

describe("P2 Advanced / Niche APIs", () => {
  test("configPath for local config", () => {
    const localPath = repo.configPath(ConfigSource.Local);
    expect(typeof localPath).toBe("string");
    expect(localPath).toContain("config");
  });

  test("revParseWith options parsing", () => {
    const res = repo.revParseWith("HEAD", {
      single: true,
      anchor: false,
    });
    expect(res).toBeDefined();
    expect(res.single).toBe(repo.headId());
  });

  test("reverseRevisionRange helper", () => {
    const range = repo.reverseRevisionRange("HEAD~1..HEAD");
    expect(typeof range).toBe("string");
    expect(range).toBe("HEAD..HEAD~1");
  });
});
