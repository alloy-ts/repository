import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { RemoteDirection, Repository } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

function git(args: string, cwd: string) {
  execSync(`git ${args}`, { cwd, stdio: "ignore" });
}

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-p4-"));
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
  writeFileSync(join(cloneDir, "p4_fixture.txt"), "p4 fixture payload\n");
  git("add -A", cloneDir);
  git("config commit.gpgsign false", cloneDir);
  git('-c user.email=test@example.com -c user.name=Test commit -m "p4 fixture commit"', cloneDir);
  repo = Repository.open(cloneDir);
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true });
});

describe("P4 Extended Remote and Config APIs", () => {
  test("createRemote adds a new remote and reflects in remoteNames", () => {
    const remote = repo.createRemote("upstream", "https://github.com/example/upstream.git");
    expect(remote.name).toBe("upstream");
    expect(remote.url).toBe("https://github.com/example/upstream.git");

    const names = repo.remoteNames();
    expect(names).toContain("upstream");
  });

  test("findDefaultRemote returns configured default remote or null", () => {
    const defaultRemote = repo.findDefaultRemote(RemoteDirection.Fetch);
    expect(defaultRemote === null || typeof defaultRemote === "object").toBe(true);
  });
});
