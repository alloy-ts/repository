import { execSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-branch-"));
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
});

afterAll(() => {
  rmSync(base, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 200,
  });
});

describe("Repository branches (delete / upstream mapping)", () => {
  test("branchNames() and branchRemoteName() list branches and tracking remotes", () => {
    const branches = repo.branchNames();
    expect(Array.isArray(branches)).toBe(true);
    for (const b of branches) {
      expect(typeof b).toBe("string");
      const remote = repo.branchRemoteName(b);
      expect(remote === null || typeof remote).toBe("string");
    }
  });

  test("deleteLocalBranches() deletes the ref and its branch config", () => {
    const head = repo.headId();
    repo.createReference("refs/heads/__probe_del__", head, true);
    repo.configSet("branch.__probe_del__.remote", "origin");
    repo.configSet("branch.__probe_del__.merge", "refs/heads/__probe_del__");

    const deleted = repo.deleteLocalBranches(["__probe_del__"]);
    expect(deleted).toEqual(["refs/heads/__probe_del__"]);

    const fresh = Repository.open(cloneDir);
    expect(fresh.branchNames()).not.toContain("__probe_del__");
    expect(fresh.configString("branch.__probe_del__.remote")).toBeNull();
    expect(repo.tryFindReference("refs/heads/__probe_del__")).toBe(null);
  });

  test("deleteLocalBranches() refuses the checked-out branch", () => {
    const current = repo.headName();
    if (!current) return;
    const short = current.replace(/^refs\/heads\//, "");
    expect(() => repo.deleteLocalBranches([short])).toThrow();
    // a non-local ref name is also rejected
    expect(() => repo.deleteLocalBranches(["refs/tags/__probe_del__"])).toThrow();
  });

  test("deleteLocalBranchesIfUnchanged() deletes on matching target, refuses a stale one", () => {
    const head = repo.headId();
    repo.createReference("refs/heads/__probe_guard__", head, true);

    expect(() =>
      repo.deleteLocalBranchesIfUnchanged([
        {
          name: "__probe_guard__",
          target: "1111111111111111111111111111111111111111",
        },
      ]),
    ).toThrow();
    expect(repo.tryFindReference("refs/heads/__probe_guard__")?.name).toBe(
      "refs/heads/__probe_guard__",
    );

    repo.deleteLocalBranchesIfUnchanged([
      {
        name: "__probe_guard__",
        target: head,
      },
    ]);
    expect(repo.tryFindReference("refs/heads/__probe_guard__")).toBe(null);
  });

  test("branchRemoteRefName / branchRemoteTrackingRefName map the current branch", () => {
    const current = repo.headName();
    if (!current) return;
    const short = current.replace(/^refs\/heads\//, "");

    expect(repo.branchRemoteRefName(short)).toBe(current);
    expect(repo.branchRemoteRefName(current)).toBe(current);
    expect(repo.branchRemoteTrackingRefName(short)).toBe(`refs/remotes/origin/${short}`);
    // unconfigured branch has no mapping
    expect(repo.branchRemoteRefName("__never_configured__")).toBe(null);
  });

  test("branchRemote() returns the configured remote; upstream mapping round-trips", () => {
    const current = repo.headName();
    if (!current) return;
    const short = current.replace(/^refs\/heads\//, "");

    const remote = repo.branchRemote(short);
    expect(remote?.name).toBe("origin");
    expect(repo.branchRemote("__never_configured__")).toBe(null);

    const upstream = repo.upstreamBranchAndRemoteForTrackingBranch(`refs/remotes/origin/${short}`);
    expect(upstream?.upstreamBranch).toBe(current);
    expect(upstream?.remoteName).toBe("origin");
    // a local branch is not a remote-tracking branch — gix rejects the category outright
    expect(() => repo.upstreamBranchAndRemoteForTrackingBranch(current)).toThrow(
      /remote tracking branch/,
    );
  });
});
