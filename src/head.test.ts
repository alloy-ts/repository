import { execSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Head, Repository } from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

// A freshly initialised repo with no commits, used for the unborn-HEAD cases.
let unbornDir: string;
let unborn: Repository;

// The canonical empty-tree id (sha1), which `headTreeIdOrEmpty` falls back to.
const EMPTY_TREE_SHA1 = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

function git(args: string, cwd: string) {
  execSync(`git -c protocol.file.allow=always ${args}`, { cwd, stdio: "ignore" });
}

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-head-"));
  cloneDir = join(base, "clone");
  try {
    repo = await Repository.clone(realRoot, cloneDir);
  } catch {
    rmSync(cloneDir, { recursive: true, force: true });
    execSync(`git clone --local --quiet ${JSON.stringify(realRoot)} ${JSON.stringify(cloneDir)}`, {
      stdio: "ignore",
    });
    repo = Repository.open(cloneDir);
  }

  unbornDir = join(base, "unborn");
  mkdirSync(unbornDir, { recursive: true });
  git("init --initial-branch=main", unbornDir);
  git("config user.email a@e.com", unbornDir);
  git("config user.name A", unbornDir);
  unborn = Repository.open(unbornDir);
});

afterAll(() => {
  rmSync(base, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 200,
  });
});

describe("Repository head (src/head.rs)", () => {
  test("points HEAD at a commit id (detached)", () => {
    const head = repo.head();
    expect(head.kind).toBe("symbolic");
    const id = repo.headId();

    repo.setHead(id);

    const detached = repo.head();
    expect(detached.kind).toBe("detached");
    expect(repo.headId()).toBe(id);
  });

  test("points HEAD at a branch name (symbolic, DWIM)", () => {
    const branch = "main";
    repo.setHead(`refs/heads/${branch}`);
    expect(repo.head().kind).toBe("symbolic");
    const referent = repo.headRef();
    expect(referent).not.toBe(null);
    expect(referent!.name).toBe(`refs/heads/${branch}`);

    // A short (DWIM) name resolves to the same referent.
    repo.setHead(branch);
    expect(repo.headRef()!.name).toBe(`refs/heads/${branch}`);
  });

  test("head() returns the HEAD struct (kind + referent)", () => {
    repo.setHead("refs/heads/main");
    const head = repo.head();
    expect(head.kind).toBe("symbolic");
    expect(head.referent).toBe("refs/heads/main");
  });

  test("headRef() returns the referent reference", () => {
    repo.setHead("refs/heads/main");
    const referent = repo.headRef();
    expect(referent).not.toBe(null);
    expect(referent!.name).toBe("refs/heads/main");
  });

  test("headName()/headPath() mirror the referent", () => {
    repo.setHead("refs/heads/main");
    expect(repo.headName()).toBe("refs/heads/main");
    expect(repo.headPath()).toBe("refs/heads/main");
  });
});

describe("Head class (src/head.rs)", () => {
  test("constructs from HeadOptions and exposes kind/referent getters", () => {
    repo.setHead("refs/heads/main");
    const head = new Head({ repo, referent: "refs/heads/main", kind: "symbolic" });
    expect(head.kind).toBe("symbolic");
    expect(head.referent).toBe("refs/heads/main");
  });

  test("live methods resolve against the repository", () => {
    repo.setHead("refs/heads/main");
    const head = repo.head();

    expect(head.id()).toBe(repo.headId());

    const ref = head.ref();
    expect(ref).not.toBe(null);
    expect(ref!.name).toBe(repo.headRef()!.name);

    expect(head.treeId()).toBe(repo.headTreeId());
    expect(head.tree().id).toBe(repo.headTree().id);
    expect(head.treeIdOrEmpty()).toBe(repo.headTreeIdOrEmpty());
  });

  test("detached() reflects the kind", () => {
    repo.setHead("refs/heads/main");
    expect(repo.head().detached()).toBe(false);

    repo.setHead(repo.headId());
    const head = repo.head();
    expect(head.kind).toBe("detached");
    expect(head.detached()).toBe(true);
  });

  test("resolution methods throw when built without a repository", () => {
    const head = new Head({
      repo: undefined,
      referent: "refs/heads/main",
      kind: "symbolic",
    });

    // detached() is derived from `kind`, so it still works without a repo.
    expect(head.detached()).toBe(false);

    expect(() => head.id()).toThrow();
    expect(() => head.ref()).toThrow();
    expect(() => head.tree()).toThrow();
    expect(() => head.treeId()).toThrow();
    expect(() => head.treeIdOrEmpty()).toThrow();
  });
});

describe("Head unborn (src/head.rs)", () => {
  test("head() is unborn and points at the initial branch", () => {
    const head = unborn.head();
    expect(head.kind).toBe("unborn");
    expect(head.referent).toBe("refs/heads/main");
    expect(head.detached()).toBe(false);
  });

  test("treeIdOrEmpty() returns the empty tree id", () => {
    const head = unborn.head();
    expect(head.treeIdOrEmpty()).toBe(EMPTY_TREE_SHA1);

    // There is no tree to point at yet, so the non-empty variant throws.
    expect(() => head.treeId()).toThrow();
  });
});
