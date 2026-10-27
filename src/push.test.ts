import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository } from "./main.ts";

let root: string;
let work: string;
let remotePath: string;
let repo: Repository;
// Commit graph used by the push scenarios, in creation order. `rewritten`
// diverges from `second`, which is what makes the plain push get rejected.
let first: string;
let second: string;
let rewritten: string;
let latest: string;

function sh(command: string, cwd = work): string {
  return execSync(command, { cwd }).toString();
}

function git(args: string) {
  execSync(`git ${args}`, { cwd: work, stdio: "ignore" });
}

/** Write a file, commit everything and return the new commit id. */
function commit(name: string, content: string, message: string): string {
  writeFileSync(join(work, name), content);
  git("add -A");
  git(`commit -m ${JSON.stringify(message)}`);
  return sh("git rev-parse HEAD").trim();
}

/** Target of `ref` on the bare `origin`, or `null` when it does not exist. */
function remoteRef(ref: string): string | null {
  try {
    return execSync(`git --git-dir=${remotePath} rev-parse ${ref}`, {
      stdio: ["ignore", "pipe", "ignore"],
    })
      .toString()
      .trim();
  } catch {
    return null;
  }
}

beforeAll(() => {
  root = mkdtempSync(join(tmpdir(), "repo-push-"));
  remotePath = join(root, "remote.git");
  work = join(root, "work");

  // A bare remote that already points HEAD at `main`, so pushes do not need
  // `-u`/`HEAD` juggling.
  execSync(`git init --bare ${JSON.stringify(remotePath)}`, {
    cwd: root,
    stdio: "ignore",
  });
  execSync(`git --git-dir=${JSON.stringify(remotePath)} symbolic-ref HEAD refs/heads/main`);

  execSync(`git init -b main ${JSON.stringify(work)}`, {
    cwd: root,
    stdio: "ignore",
  });
  git("config user.email test@example.com");
  git("config user.name Test");
  git("config commit.gpgsign false");
  git(`remote add origin ${JSON.stringify(remotePath)}`);

  first = commit("a.txt", "one\n", "one");
  second = commit("b.txt", "two\n", "two");
  // Rewind and commit something unrelated: pushing this is not a fast-forward.
  git(`reset --hard ${first}`);
  rewritten = commit("a.txt", "rewritten\n", "rewritten");
  latest = commit("c.txt", "three\n", "three");

  repo = Repository.open(work);
});

afterAll(() => {
  rmSync(root, { recursive: true, force: true });
});

describe("Repository.push (src/push.rs)", () => {
  test("push() creates the branch on an empty remote", () => {
    // The local branch is at `latest`, which descends from `rewritten`; the
    // remote is empty, so this is a brand-new ref.
    const outcome = repo.push(["refs/heads/main"]);

    expect(outcome.ok).toBe(true);
    expect(outcome.updates).toHaveLength(1);
    expect(outcome.updates[0]).toMatchObject({
      srcRefname: "refs/heads/main",
      dstRefname: "refs/heads/main",
      forced: false,
      deleted: false,
      status: "new",
    });
    expect(remoteRef("refs/heads/main")).toBe(latest);
  });

  test("push() reports up-to-date when the remote already matches", () => {
    const outcome = repo.push(["refs/heads/main"]);

    expect(outcome.ok).toBe(true);
    expect(outcome.updates[0]).toMatchObject({
      dstRefname: "refs/heads/main",
      status: "up-to-date",
    });
    expect(outcome.updates[0].src ?? undefined).toBeUndefined();
    expect(outcome.updates[0].dst ?? undefined).toBeUndefined();
  });

  test("push() reports a fast-forward with abbreviated oids", () => {
    const ahead = commit("d.txt", "four\n", "four");
    const outcome = repo.push(["refs/heads/main"]);

    expect(outcome.ok).toBe(true);
    const update = outcome.updates[0];
    expect(update.status).toBe("fast-forward");
    expect(update.forced).toBe(false);
    expect(update.deleted).toBe(false);
    expect(latest.startsWith(update.src!)).toBe(true);
    expect(ahead.startsWith(update.dst!)).toBe(true);
    expect(remoteRef("refs/heads/main")).toBe(ahead);
    latest = ahead;
  });

  test("push() reports a non-fast-forward rejection without throwing", () => {
    // Move the local branch to `rewritten`, which does not descend from the
    // remote tip.
    git(`reset --hard ${rewritten}`);
    const outcome = repo.push(["refs/heads/main"]);

    expect(outcome.ok).toBe(false);
    expect(outcome.updates).toHaveLength(1);
    expect(outcome.updates[0]).toMatchObject({
      status: "rejected",
      forced: false,
      deleted: false,
    });
    expect(outcome.updates[0].message).toBe("non-fast-forward");
    // The remote must be untouched by a rejected push.
    expect(remoteRef("refs/heads/main")).toBe(latest);
  });

  test("push() with force overwrites the remote and flags the update as forced", () => {
    const outcome = repo.push(["refs/heads/main"], { force: true });

    expect(outcome.ok).toBe(true);
    expect(outcome.updates[0]).toMatchObject({
      status: "forced",
      forced: true,
    });
    expect(outcome.updates[0].message).toBe("forced update");
    expect(remoteRef("refs/heads/main")).toBe(rewritten);
  });

  test("push() with dryRun reports the update but leaves the remote alone", () => {
    const ahead = commit("e.txt", "five\n", "five");
    const outcome = repo.push(["refs/heads/main"], { dryRun: true });

    expect(outcome.ok).toBe(true);
    expect(outcome.updates[0].status).toBe("fast-forward");
    expect(outcome.updates[0].dst!.startsWith(ahead.slice(0, 7))).toBe(true);
    expect(remoteRef("refs/heads/main")).toBe(rewritten);
  });

  test("push() reports every refspec in one call", () => {
    git(`branch topic ${first}`);
    const outcome = repo.push(["refs/heads/main", "refs/heads/topic"]);

    expect(outcome.ok).toBe(true);
    expect(outcome.updates.map((u) => u.dstRefname).sort()).toEqual([
      "refs/heads/main",
      "refs/heads/topic",
    ]);
    expect(outcome.updates.find((u) => u.dstRefname === "refs/heads/topic")!.status).toBe("new");
  });

  test("push() deletes a remote branch with delete: true", () => {
    const outcome = repo.push(["refs/heads/topic"], { delete: true });

    expect(outcome.ok).toBe(true);
    expect(outcome.updates[0]).toMatchObject({
      dstRefname: "refs/heads/topic",
      deleted: true,
      status: "deleted",
    });
    // A deletion has no source side.
    expect(outcome.updates[0].srcRefname ?? undefined).toBeUndefined();
    expect(remoteRef("refs/heads/topic")).toBeNull();
  });

  test("push() throws when the remote does not exist", () => {
    expect(() => repo.push(["refs/heads/main"], { remote: "no-such-remote" })).toThrow();
  });

  test("push() with no args pushes HEAD and sets upstream by default", () => {
    git(`checkout -b feature-a`);
    commit("fa.txt", "fa\n", "fa");
    const outcome = repo.push();
    expect(outcome.ok).toBe(true);
    expect(remoteRef("refs/heads/feature-a")).toBeTruthy();
    // `setUpstream` defaults to true on the branch-centric path (`git push -u`).
    expect(sh("git config --get branch.feature-a.remote").trim()).toBe("origin");
    expect(sh("git config --get branch.feature-a.merge").trim()).toBe("refs/heads/feature-a");
  });

  test("push() honors remoteBranch to rename the destination ref", () => {
    git(`checkout -b feature-b ${first}`);
    commit("fb.txt", "fb\n", "fb");
    const outcome = repo.push({ remoteBranch: "renamed-b" });
    expect(outcome.ok).toBe(true);
    expect(remoteRef("refs/heads/renamed-b")).toBeTruthy();
    expect(remoteRef("refs/heads/feature-b")).toBeNull();
    expect(sh("git config --get branch.feature-b.remote").trim()).toBe("origin");
    expect(sh("git config --get branch.feature-b.merge").trim()).toBe("refs/heads/renamed-b");
  });

  test("push() with setUpstream:false leaves tracking unconfigured", () => {
    git(`checkout -b feature-c`);
    commit("fc.txt", "fc\n", "fc");
    const outcome = repo.push({ setUpstream: false });
    expect(outcome.ok).toBe(true);
    expect(() => sh("git config --get branch.feature-c.remote")).toThrow();
  });

  test("push() with explicit refspecs does not set upstream", () => {
    git(`checkout -b feature-d`);
    commit("fd.txt", "fd\n", "fd");
    const outcome = repo.push(["refs/heads/feature-d"]);
    expect(outcome.ok).toBe(true);
    // Raw refspecs opt out of upstream tracking (default false for that path).
    expect(() => sh("git config --get branch.feature-d.remote")).toThrow();
  });

  test("pushBranch() is a branch-centric shortcut that sets upstream", () => {
    git(`checkout -b feature-e`);
    commit("fe.txt", "fe\n", "fe");
    const outcome = repo.pushBranch();
    expect(outcome.ok).toBe(true);
    expect(remoteRef("refs/heads/feature-e")).toBeTruthy();
    expect(sh("git config --get branch.feature-e.remote").trim()).toBe("origin");
    expect(sh("git config --get branch.feature-e.merge").trim()).toBe("refs/heads/feature-e");
  });
});
