import { execSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "vite-plus/test";
import { Repository, setupRemotes } from "../dist/index.js";

const MANIFEST = [
  "name: test",
  "version: 0.0.0",
  "type: application",
  "owner: test",
  "description: Test manifest.",
  "config:",
  "  - commit.gpgsign: false",
  "remote:",
  "  - https://repo.example.com/acme/test",
  "  - https://github.com/acme/test",
  "  - https://gitlab.com/acme/test",
  "  - https://dev.azure.com/acme/test",
  "",
].join("\n");

/** A fresh repo with the manifest committed, so it is reachable from `HEAD`. */
function setup(): { dir: string; repo: Repository } {
  const dir = mkdtempSync(join(tmpdir(), "util-test-"));
  const repo = Repository.init(dir);
  writeFileSync(join(dir, "repository.manifest.yml"), MANIFEST);
  // Commit the manifest so `setupRemotes(dir, "HEAD")` can read it from git history.
  execSync(`git -C ${JSON.stringify(dir)} add repository.manifest.yml`, {
    stdio: "ignore",
  });
  execSync(
    `git -C ${JSON.stringify(dir)} -c user.name=test -c user.email=test@t -c commit.gpgsign=false commit -m init`,
    { stdio: "ignore" },
  );
  return { dir, repo };
}

function gitConfig(dir: string): string {
  return readFileSync(join(dir, ".git", "config"), "utf8");
}

test("setupRemotes derives remote names from the manifest URLs", () => {
  const { dir } = setup();
  try {
    const names = setupRemotes(dir, "HEAD");

    // First URL -> origin; others named by host domain label (4 URLs -> 4 remotes).
    expect(names).toEqual(["origin", "github", "gitlab", "azure"]);

    // Remotes are persisted and discoverable on a fresh open (order is gix's, not manifest's).
    const reopened = Repository.open(dir);
    expect(reopened.remotes().map((r) => r.name).sort()).toEqual([...names].sort());
    expect(reopened.findRemote("origin").url).toBe(
      "https://repo.example.com/acme/test",
    );
    expect(reopened.findRemote("github").url).toBe(
      "https://github.com/acme/test",
    );
    expect(reopened.findRemote("azure").url).toBe(
      "https://dev.azure.com/acme/test",
    );
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("setupRemotes writes url + fetch refspec into .git/config", () => {
  const { dir } = setup();
  try {
    setupRemotes(dir, "HEAD");
    const config = gitConfig(dir);

    expect(config).toContain('[remote "origin"]');
    expect(config).toContain("url = https://repo.example.com/acme/test");
    expect(config).toContain("fetch = +refs/heads/*:refs/remotes/origin/*");
    expect(config).toContain('[remote "github"]');
    expect(config).toContain("fetch = +refs/heads/*:refs/remotes/github/*");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("setupRemotes applies manifest config entries", () => {
  const { dir } = setup();
  try {
    setupRemotes(dir, "HEAD");
    // `config: [- commit.gpgsign: false]` landed in the repo config.
    expect(Repository.open(dir).configString("commit.gpgsign")).toBe("false");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("setupRemotes is idempotent", () => {
  const { dir } = setup();
  try {
    const first = setupRemotes(dir, "HEAD");
    const second = setupRemotes(dir, "HEAD");
    expect(second).toEqual(first);

    // Existing sections are rewritten, not duplicated.
    const config = gitConfig(dir);
    expect(config.match(/\[remote "origin"\]/g)?.length).toBe(1);
    const githubUrls = config.match(/url = https:\/\/github\.com\/acme\/test/g);
    expect(githubUrls?.length).toBe(1);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("setupRemotes surfaces a clear error for a missing manifest path", () => {
  const { dir } = setup();
  try {
    // HEAD has `repository.manifest.yml`, but not `nope.yml`.
    expect(() => setupRemotes(dir, "HEAD", "nope.yml")).toThrow(/no .*nope\.yml/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
