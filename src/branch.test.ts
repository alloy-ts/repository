import { execSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { branchNameIsValid, Repository } from "./main.ts";

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

describe("Repository branches (create / find / rename / upstream)", () => {
  const head = () => repo.headId();

  test("localBranches() and remoteBranches() list branch references", () => {
    const locals = repo.localBranches();
    expect(locals.length).toBeGreaterThan(0);
    for (const b of locals) {
      expect(b.category()).toBe("local_branch");
      expect(b.name.startsWith("refs/heads/")).toBe(true);
    }
    const remotes = repo.remoteBranches();
    expect(remotes.length).toBeGreaterThan(0);
    for (const b of remotes) {
      expect(b.category()).toBe("remote_branch");
      expect(b.name.startsWith("refs/remotes/")).toBe(true);
    }
  });

  test("findBranch() / tryFindBranch() resolve short and full names", () => {
    const id = head();
    repo.createBranch("__probe_find__", id, true);

    expect(repo.findBranch("__probe_find__").target).toBe(id);
    expect(repo.findBranch("refs/heads/__probe_find__").name).toBe("refs/heads/__probe_find__");
    expect(repo.tryFindBranch("__probe_find__", "local")?.target).toBe(id);
    expect(repo.tryFindBranch("__never_created__")).toBe(null);
    expect(() => repo.findBranch("__never_created__")).toThrow();

    const remote = repo.remoteBranches()[0]?.name;
    if (remote) {
      expect(repo.tryFindBranch(remote, "remote")?.name).toBe(remote);
      // the same name is not reachable as a local branch
      expect(() => repo.findBranch(remote, "local")).toThrow();
    }
    expect(() => repo.findBranch("__probe_find__", "nonsense")).toThrow();
  });

  test("createBranch() points at an object id or revision and refuses duplicates", () => {
    const id = head();
    expect(repo.createBranch("__probe_dup__", id).target).toBe(id);
    expect(() => repo.createBranch("__probe_dup__", id)).toThrow();
    expect(repo.createBranch("__probe_dup__", id, true).target).toBe(id);
    expect(repo.createBranch("__probe_rev__", "HEAD", true).target).toBe(id);
  });

  test("branchIsHead() is true only for the checked out branch", () => {
    const current = repo.headName();
    if (!current) return;
    expect(repo.branchIsHead(current)).toBe(true);
    expect(repo.branchIsHead("__probe_dup__")).toBe(false);
  });

  test("renameBranch() moves the ref and its branch configuration", () => {
    const id = head();
    repo.createBranch("__probe_ren__", id, true);
    repo.configSet("branch.__probe_ren__.remote", "origin");
    repo.configSet("branch.__probe_ren__.merge", "refs/heads/__probe_ren__");
    repo.configSet("branch.__probe_ren__.description", "keep me");

    const renamed = repo.renameBranch("__probe_ren__", "__probe_ren2__");
    expect(renamed.name).toBe("refs/heads/__probe_ren2__");
    expect(renamed.target).toBe(id);
    expect(repo.tryFindBranch("__probe_ren__")).toBe(null);

    const fresh = Repository.open(cloneDir);
    expect(fresh.configString("branch.__probe_ren2__.remote")).toBe("origin");
    expect(fresh.configString("branch.__probe_ren2__.description")).toBe("keep me");
    expect(fresh.configString("branch.__probe_ren__.remote")).toBeNull();

    // overwriting an existing branch requires force
    expect(() => repo.renameBranch("__probe_ren2__", "__probe_dup__")).toThrow();
  });

  test("setBranchUpstream() sets and unsets the upstream", () => {
    const id = head();
    const current = repo.headName();
    if (!current) return;
    const short = current.replace(/^refs\/heads\//, "");

    // a branch without a remote cannot take a plain branch name as upstream
    repo.createBranch("__probe_noup__", id, true);
    expect(() => repo.setBranchUpstream("__probe_noup__", short)).toThrow();

    repo.configSet("branch.__probe_noup__.remote", "origin");
    repo.setBranchUpstream("__probe_noup__", short);
    // configuration is cached when the repository is opened, so reads happen on a reopen
    const fresh = Repository.open(cloneDir);
    expect(fresh.configString("branch.__probe_noup__.merge")).toBe(current);
    expect(fresh.branchRemoteTrackingRefName("__probe_noup__")).toBe(
      `refs/remotes/origin/${short}`,
    );

    // the checked-out branch already has `branch.<name>.remote` from the clone
    repo.setBranchUpstream(short, "main");
    expect(Repository.open(cloneDir).configString(`branch.${short}.merge`)).toBe("refs/heads/main");

    // a remote-tracking branch carries the remote along
    repo.setBranchUpstream(short, `origin/${short}`);
    expect(Repository.open(cloneDir).configString(`branch.${short}.merge`)).toBe(current);

    // `null` unsets the upstream
    repo.setBranchUpstream(short, null);
    const cleared = Repository.open(cloneDir);
    expect(cleared.configString(`branch.${short}.merge`)).toBeNull();
    expect(cleared.configString(`branch.${short}.remote`)).toBeNull();
  });

  test("branchRemoteName() accepts full names and a direction", () => {
    const current = repo.headName();
    if (!current) return;
    expect(repo.branchRemoteName(current)).toBe("origin");
    expect(repo.branchRemoteName(current, "fetch")).toBe("origin");
    expect(repo.branchRemoteName("__never_configured__")).toBeNull();
  });

  test("branchNameIsValid() accepts branch names git accepts", () => {
    expect(branchNameIsValid("topic")).toBe(true);
    expect(branchNameIsValid("feature/topic")).toBe(true);
    expect(branchNameIsValid("")).toBe(false);
    expect(branchNameIsValid("with spaces")).toBe(false);
    expect(branchNameIsValid("~tilde")).toBe(false);
  });
});

describe("Repository branches (listBranches convenience)", () => {
  test("listBranches() returns structured items with tip time and HEAD flag", () => {
    const current = repo.headName();
    const items = repo.listBranches();
    expect(items.length).toBeGreaterThan(0);
    for (const it of items) {
      expect(typeof it.name).toBe("string");
      expect(it.fullName.startsWith("refs/heads/")).toBe(true);
      expect(it.target).toMatch(/^[0-9a-f]{40}$/);
      expect(typeof it.commitTime.seconds).toBe("number");
      expect(typeof it.commitTime.offset).toBe("number");
      expect(typeof it.isHead).toBe("boolean");
    }
    if (current) {
      const headItem = items.find((it) => it.fullName === current);
      expect(headItem).toBeDefined();
      expect(headItem!.isHead).toBe(true);
      // the checked-out branch has an upstream from the clone, so it is reported
      expect(headItem!.upstream).toMatch(/^refs\/remotes\//);
    }
    // the remote variant lists remote-tracking refs
    const remotes = repo.listBranches("remote");
    expect(remotes.length).toBeGreaterThan(0);
    expect(remotes[0].fullName.startsWith("refs/remotes/")).toBe(true);
  });
});
