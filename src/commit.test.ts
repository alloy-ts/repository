import { execSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";

import { Commit, format_trailer, format_trailers, Repository, Signature } from "./main.ts";

let fixture: string;
let repo: Repository;

// Local repo setup only (no network); let the shell resolve `git`.
function git(args: string, cwd: string) {
  execSync(`git -c protocol.file.allow=always ${args}`, {
    cwd,
    stdio: "ignore",
  });
}

const RFC3339 = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}/;

beforeAll(() => {
  fixture = mkdtempSync(join(tmpdir(), "repository-commit-"));
  const work = join(fixture, "work");
  mkdirSync(work, { recursive: true });
  git("init --initial-branch=main", work);
  git("config user.email a@e.com", work);
  git("config user.name A", work);
  git("config commit.gpgsign false", work);

  // First commit with a subject + body and an explicit author date.
  writeFileSync(join(work, "a.txt"), "a\n");
  git("add a.txt", work);
  git('commit -m "Subject line" -m "Body paragraph one"', work);

  // Second commit so we have a first-parent chain for nthAncestor.
  writeFileSync(join(work, "b.txt"), "b\n");
  git("add b.txt", work);
  git("commit -m second", work);

  repo = Repository.open(work);
});

afterAll(() => {
  rmSync(fixture, { recursive: true, force: true });
});

describe("CommitInfo (Repository.log)", () => {
  test("enriched fields: sha, shortSha, subject, body, dates", () => {
    const entries = repo.log();
    expect(entries.length).toBeGreaterThanOrEqual(2);

    const head = entries[0];
    expect(head.sha).toMatch(/^[0-9a-f]{40}$/);
    expect(head.shortSha.length).toBeGreaterThanOrEqual(7);
    expect(head.sha.startsWith(head.shortSha)).toBe(true);
    expect(head.subject).toBe("second");
    expect(head.body).toBe("");
    expect(RFC3339.test(head.authorDate)).toBe(true);
    expect(RFC3339.test(head.committerDate)).toBe(true);
    expect(head.parentIds.length).toBe(1);

    const first = entries[1];
    expect(first.subject).toBe("Subject line");
    expect(first.body).toBe("Body paragraph one");
  });
});

describe("Commit handle getters", () => {
  test("shortSha / subject / authorDate / committerDate", () => {
    const entries = repo.log();
    const head = entries[0];
    const commit: Commit = repo.findCommit(head.sha);

    expect(head.sha.startsWith(commit.shortSha())).toBe(true);
    expect(commit.subject()).toBe("second");
    expect(RFC3339.test(commit.authorDate())).toBe(true);
    expect(RFC3339.test(commit.committerDate())).toBe(true);
  });

  test("nthAncestor follows first-parent history", () => {
    const entries = repo.log();
    const head = entries[0];
    const commit: Commit = repo.findCommit(head.sha);
    const parent = commit.nthAncestor(1);
    expect(parent.id()).toBe(entries[1].sha);
    // nthAncestor(0) is the commit itself.
    expect(commit.nthAncestor(0).id()).toBe(head.sha);
  });

  test("nthAncestor past the root errors", () => {
    const entries = repo.log();
    const root = repo.findCommit(entries[entries.length - 1].sha);
    expect(() => root.nthAncestor(1)).toThrow();
  });

  test("info() mirrors the CommitInfo shape from log()", () => {
    const entries = repo.log();
    const commit: Commit = repo.findCommit(entries[0].sha);
    const info = commit.info();
    expect(info.sha).toBe(entries[0].sha);
    expect(info.subject).toBe(entries[0].subject);
    expect(info.body).toBe(entries[0].body);
    expect(info.shortSha.length).toBeGreaterThanOrEqual(7);
    expect(info.sha.startsWith(info.shortSha)).toBe(true);
    expect(RFC3339.test(info.authorDate)).toBe(true);
    expect(RFC3339.test(info.committerDate)).toBe(true);
  });
});

describe("commitAll / addAll (high-level commit)", () => {
  let dir: string;
  let cr: Repository;

  beforeAll(() => {
    dir = mkdtempSync(join(tmpdir(), "repository-commitall-"));
    git("init --initial-branch=main", dir);
    git("config user.email a@e.com", dir);
    git("config user.name A", dir);
    git("config commit.gpgsign false", dir);
    writeFileSync(join(dir, "seed.txt"), "seed\n");
    git("add seed.txt", dir);
    git("commit -m seed", dir);
    cr = Repository.open(dir);
  });

  afterAll(() => {
    rmSync(dir, { recursive: true, force: true });
  });

  test("commitAll stages the worktree and commits on HEAD", () => {
    writeFileSync(join(dir, "c.txt"), "c\n");
    const id = cr.commitAll({ message: "commit all" });
    expect(id).toMatch(/^[0-9a-f]{40}$/);
    expect(cr.headCommit().id).toBe(id);
    expect(cr.log()[0].subject).toBe("commit all");
    expect(cr.headCommit().treeId).toMatch(/^[0-9a-f]{40}$/);
  });

  test("commitAll honors explicit author/committer and reference", () => {
    writeFileSync(join(dir, "d.txt"), "d\n");
    const author: Signature = {
      name: "Author",
      email: "author@e.com",
      time: 1700000000,
      timezoneOffset: 0,
    };
    const committer: Signature = {
      name: "Committer",
      email: "committer@e.com",
      time: 1700000001,
      timezoneOffset: 0,
    };
    const id = cr.commitAll({
      message: "with identities",
      author,
      committer,
      reference: "refs/heads/main",
    });
    const commit = cr.findCommit(id);
    expect(commit.author().name).toBe("Author");
    expect(commit.author().email).toBe("author@e.com");
    expect(commit.committer().name).toBe("Committer");
    expect(cr.headCommit().id).toBe(id);
  });

  test("commitAll(all:false) commits the already-staged index", () => {
    writeFileSync(join(dir, "e.txt"), "e\n");
    const idx = cr.index();
    cr.addAll(idx);
    idx.write();
    const id = cr.commitAll({ message: "staged", all: false });
    expect(cr.headCommit().id).toBe(id);
    expect(cr.log()[0].subject).toBe("staged");
  });

  test("commitAllAsync resolves to a new commit id", async () => {
    writeFileSync(join(dir, "f.txt"), "f\n");
    const id = await cr.commitAllAsync({ message: "async commit" });
    expect(id).toMatch(/^[0-9a-f]{40}$/);
    expect(cr.headCommit().id).toBe(id);
    expect(cr.log()[0].subject).toBe("async commit");
  });

  test("addAll stages every worktree file into the index", () => {
    writeFileSync(join(dir, "g1.txt"), "g1\n");
    writeFileSync(join(dir, "g2.txt"), "g2\n");
    const idx = cr.index();
    const before = idx.len();
    cr.addAll(idx);
    expect(idx.len()).toBe(before + 2);
    const paths = idx.entries().map((e) => e.path);
    expect(paths).toContain("g1.txt");
    expect(paths).toContain("g2.txt");
  });
});

describe("writeCommit (loose object) with headers / encoding", () => {
  test("writeCommit preserves encoding and extra headers; toObjectText round-trips", () => {
    const head = repo.headCommit();
    const oid = repo.writeCommit({
      tree: head.treeId,
      parents: head.parentIds,
      author: head.author(),
      committer: head.committer(),
      message: "object-form commit\n\nReviewed-by: X <x@e.com>\n",
      encoding: "UTF-8",
      extraHeaders: [{ name: "Reviewed-by", value: "Reviewer <r@e.com>" }],
    });
    expect(oid).toMatch(/^[0-9a-f]{40}$/);

    const c: Commit = repo.findCommit(oid);
    expect(c.encoding()).toBe("UTF-8");
    expect(c.headerField("Reviewed-by")).toBe("Reviewer <r@e.com>");
    expect(c.extraHeaders().map((h) => h.name)).toContain("Reviewed-by");

    const text = c.toObjectText();
    expect(text).toContain("encoding UTF-8");
    expect(text).toContain("Reviewed-by: Reviewer <r@e.com>");
    expect(text).toContain("object-form commit");
  });

  test("writeCommit without headers/encoding writes a plain commit", () => {
    const head = repo.headCommit();
    const oid = repo.writeCommit({
      tree: head.treeId,
      parents: head.parentIds,
      author: head.author(),
      committer: head.committer(),
      message: "plain loose commit",
    });
    const c: Commit = repo.findCommit(oid);
    expect(c.encoding()).toBeNull();
    expect(c.extraHeaders().length).toBe(0);
    expect(c.toObjectText()).toContain("plain loose commit");
  });
});

describe("Trailer rendering (format_trailer / format_trailers)", () => {
  test("format_trailer renders token + separator + value", () => {
    const t = { token: "Signed-off-by", value: "A <a@e.com>" };
    expect(format_trailer(t)).toBe("Signed-off-by: A <a@e.com>");
    expect(format_trailer(t, "=>")).toBe("Signed-off-by=>A <a@e.com>");
  });

  test("format_trailers joins trailers one per line with the separator", () => {
    const trailers = [
      { token: "Co-authored-by", value: "B <b@e.com>" },
      { token: "Reviewed-by", value: "C <c@e.com>" },
    ];
    expect(format_trailers(trailers)).toBe("Co-authored-by: B <b@e.com>\nReviewed-by: C <c@e.com>");
    expect(format_trailers(trailers, "=")).toBe(
      "Co-authored-by=B <b@e.com>\nReviewed-by=C <c@e.com>",
    );
  });

  test("round-trips a commit's parsed trailers back to text", () => {
    const head = repo.headCommit();
    const oid = repo.writeCommit({
      tree: head.treeId,
      parents: head.parentIds,
      author: head.author(),
      committer: head.committer(),
      message: "with trailer\n\nSigned-off-by: A <a@e.com>\nReviewed-by: B <b@e.com>\n",
    });
    const trailers = repo.findCommit(oid).trailers();
    expect(trailers.length).toBe(2);
    const text = format_trailers(trailers);
    expect(text).toContain("Signed-off-by: A <a@e.com>");
    expect(text).toContain("Reviewed-by: B <b@e.com>");
    expect(format_trailer(trailers[0])).toBe("Signed-off-by: A <a@e.com>");
  });
});
