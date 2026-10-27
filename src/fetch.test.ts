import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, test } from "vite-plus/test";
import { Remote, Repository } from "./main.ts";

// Fetch needs a remote. We build a local "upstream" repo with two branches
// (`main` and `feature`) and, per test, a fresh "local" repo that has `origin`
// pointing at the upstream. This keeps each test's assertions isolated and
// avoids any network access — gix's native fetch is exercised over a local
// filesystem path.

let upstream: string;
let local: string;
let repo: Repository;

function git(dir: string, args: string) {
  // `protocol.file.allow=always` permits the local-path transport.
  execSync(`git -c protocol.file.allow=always ${args}`, {
    cwd: dir,
    stdio: "ignore",
  });
}

function hasRef(dir: string, name: string): boolean {
  try {
    execSync(`git rev-parse --verify ${name}`, { cwd: dir, stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
}

beforeAll(() => {
  upstream = mkdtempSync(join(tmpdir(), "repo-fetch-up-"));
  git(upstream, "init --initial-branch=main");
  git(upstream, "config user.email test@example.com");
  git(upstream, "config user.name Test");
  git(upstream, "config commit.gpgsign false");

  writeFileSync(join(upstream, "a.txt"), "main\n");
  git(upstream, "add a.txt");
  git(upstream, "commit -m main");

  git(upstream, "checkout -b feature");
  writeFileSync(join(upstream, "b.txt"), "feature\n");
  git(upstream, "add b.txt");
  git(upstream, "commit -m feature");
});

beforeEach(() => {
  local = mkdtempSync(join(tmpdir(), "repo-fetch-local-"));
  git(local, "init --initial-branch=main");
  git(local, "config user.email test@example.com");
  git(local, "config user.name Test");
  git(local, "config commit.gpgsign false");
  git(local, `remote add origin ${upstream}`);
  repo = Repository.open(local);
});

afterEach(() => {
  rmSync(local, { recursive: true, force: true });
});

afterAll(() => {
  rmSync(upstream, { recursive: true, force: true });
});

describe("Repository / Remote fetch (src/fetch.rs)", () => {
  test("fetch() pulls all configured refspecs (additive defaults)", async () => {
    const outcome = await repo.fetch("origin");

    // A pack was received and refs were written for both upstream branches.
    expect(outcome.status).toBe("change");
    expect(outcome.packReceived).toBe(true);
    expect(hasRef(local, "refs/remotes/origin/main")).toBe(true);
    expect(hasRef(local, "refs/remotes/origin/feature")).toBe(true);

    // updatedRefs reports what changed; the newly created main tracking ref
    // is among them.
    expect(Array.isArray(outcome.updatedRefs)).toBe(true);
    const mainUpdate = outcome.updatedRefs.find((r) => (r.name ?? "").endsWith("origin/main"));
    expect(mainUpdate).toBeTruthy();
    expect(mainUpdate!.action.length).toBeGreaterThan(0);
  });

  test("fetchRefspec() replaces defaults and fetches only the given refspec", async () => {
    const outcome = await repo.fetchRefspec("origin", "refs/heads/main:refs/remotes/origin/main");

    expect(outcome.status).toBe("change");
    expect(outcome.packReceived).toBe(true);
    // The explicit refspec is fetched…
    expect(hasRef(local, "refs/remotes/origin/main")).toBe(true);
    // …but the remote's default `+refs/heads/*` refspec was NOT applied, so
    // the feature branch is left untouched.
    expect(hasRef(local, "refs/remotes/origin/feature")).toBe(false);
  });

  test("fetchRefspec() accepts an array of refspecs", async () => {
    const outcome = await repo.fetchRefspec("origin", [
      "refs/heads/main:refs/remotes/origin/main",
      "refs/heads/feature:refs/remotes/origin/feature",
    ]);

    expect(outcome.status).toBe("change");
    // Exactly the two requested refs were fetched.
    expect(hasRef(local, "refs/remotes/origin/main")).toBe(true);
    expect(hasRef(local, "refs/remotes/origin/feature")).toBe(true);
  });

  test("Remote.fetchRefspec() works on the Remote object directly", async () => {
    const origin: Remote = repo.findRemote("origin");
    const outcome = await origin.fetchRefspec("refs/heads/main:refs/remotes/origin/main");

    expect(outcome.status).toBe("change");
    expect(hasRef(local, "refs/remotes/origin/main")).toBe(true);
    expect(hasRef(local, "refs/remotes/origin/feature")).toBe(false);
  });

  test("fetch() honours dryRun: no refs are written", async () => {
    const outcome = await repo.fetch("origin", { dryRun: true });

    expect(outcome.dryRun).toBe(true);
    expect(outcome.status).toBe("noPackReceived");
    expect(outcome.packReceived).toBe(false);
    // Nothing was materialised into the local ref store.
    expect(hasRef(local, "refs/remotes/origin/main")).toBe(false);
    expect(hasRef(local, "refs/remotes/origin/feature")).toBe(false);
  });
});
