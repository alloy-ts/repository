import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "vite-plus/test";
import { Repository, type Signature } from "./main.ts";

const NULL = "0000000000000000000000000000000000000000";

function tempRepo(): Repository {
  const dir = mkdtempSync(join(tmpdir(), "reflog-"));
  return Repository.init(dir, false);
}

function committer(name: string): Signature {
  return {
    name,
    email: `${name}@example.com`,
    time: 1234567890,
    timezoneOffset: 0,
  };
}

test("openReflog + append + reads", () => {
  const repo = tempRepo();
  const reflog = repo.openReflog("HEAD");
  expect(reflog.name).toBe("HEAD");

  reflog.append(NULL, committer("Test"), "manual entry");

  expect(repo.reflogExists("HEAD")).toBe(true);
  expect(reflog.isEmpty()).toBe(false);
  expect(reflog.len()).toBeGreaterThanOrEqual(1);

  const recent = reflog.get(0);
  expect(recent).not.toBeNull();
  expect(recent!.subject).toBe("manual entry");
  expect(recent!.signature.name).toBe("Test");
  expect(recent!.signature.time).toBe(1234567890);
  expect(recent!.newOid).toBe(NULL);

  // entries() is oldest-first; the manual entry we appended is present.
  const all = reflog.entries();
  expect(all.some((e) => e.subject === "manual entry")).toBe(true);
});

test("deleteReflog removes the reflog file", () => {
  const repo = tempRepo();
  const reflog = repo.openReflog("refs/heads/feature");
  reflog.append(NULL, committer("X"), "feat work");
  expect(repo.reflogExists("refs/heads/feature")).toBe(true);
  expect(reflog.len()).toBe(1);

  repo.deleteReflog("refs/heads/feature");

  expect(repo.reflogExists("refs/heads/feature")).toBe(false);
  expect(reflog.isEmpty()).toBe(true);
});

test("renameReflog moves the reflog file", () => {
  const repo = tempRepo();
  const src = repo.openReflog("refs/heads/a");
  src.append(NULL, committer("Y"), "on a");
  expect(repo.reflogExists("refs/heads/a")).toBe(true);

  repo.renameReflog("refs/heads/a", "refs/heads/b");

  expect(repo.reflogExists("refs/heads/a")).toBe(false);
  expect(repo.reflogExists("refs/heads/b")).toBe(true);
  expect(repo.openReflog("refs/heads/b").len()).toBe(1);
});

test("derived fields: hash, abbreviatedHash, selector, action, message", () => {
  const repo = tempRepo();
  const reflog = repo.openReflog("HEAD");
  reflog.append(NULL, committer("Test"), "manual entry");

  const recent = reflog.get(0)!;
  // hash aliases newOid; abbreviatedHash is its first 7 hex chars.
  expect(recent.hash).toBe(recent.newOid);
  expect(recent.abbreviatedHash).toBe(recent.hash.slice(0, 7));
  // Selector is refname@{i}, counting down from the newest entry.
  expect(recent.selector).toMatch(/^HEAD@\{\d+\}$/);
  // A subject with no ": " has the whole text as subject/action and empty message.
  expect(recent.subject).toBe("manual entry");
  expect(recent.action).toBe("manual entry");
  expect(recent.message).toBe("");
});

test("action/message split on the first ': '", () => {
  const repo = tempRepo();
  repo.openReflog("HEAD").append(NULL, committer("Test"), "commit: add foo");

  const recent = repo.openReflog("HEAD").get(0)!;
  expect(recent.subject).toBe("commit: add foo");
  expect(recent.action).toBe("commit");
  expect(recent.message).toBe("add foo");
});

test("reflog() defaults to HEAD like `git reflog show`", () => {
  const repo = tempRepo();
  repo.openReflog("HEAD").append(NULL, committer("Test"), "x");
  expect(Array.isArray(repo.reflog())).toBe(true);
  expect(repo.reflog().length).toBe(repo.reflog("HEAD").length);
});

test("async reflog variants match their sync counterparts", async () => {
  const repo = tempRepo();
  repo.openReflog("HEAD").append(NULL, committer("Test"), "x");

  const sync = repo.reflog("HEAD");
  const async = await repo.reflogAsync("HEAD");
  expect(async).toEqual(sync);
  expect(await repo.reflogExistsAsync("HEAD")).toBe(repo.reflogExists("HEAD"));
});

test("async deleteReflog/renameReflog match their sync counterparts", async () => {
  const repo = tempRepo();
  repo.openReflog("refs/heads/feat").append(NULL, committer("Z"), "work");
  expect(repo.reflogExists("refs/heads/feat")).toBe(true);

  await repo.deleteReflogAsync("refs/heads/feat");
  expect(repo.reflogExists("refs/heads/feat")).toBe(false);

  repo.openReflog("refs/heads/src").append(NULL, committer("Z"), "work");
  await repo.renameReflogAsync("refs/heads/src", "refs/heads/dst");
  expect(repo.reflogExists("refs/heads/src")).toBe(false);
  expect(repo.reflogExists("refs/heads/dst")).toBe(true);
});
