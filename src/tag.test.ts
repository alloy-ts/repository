import { execSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";

import { Repository, Tag, TagInfo, TagKind } from "./main.ts";

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
  fixture = mkdtempSync(join(tmpdir(), "repository-tag-"));
  const work = join(fixture, "work");
  mkdirSync(work, { recursive: true });
  git("init --initial-branch=main", work);
  git("config user.email a@e.com", work);
  git("config user.name A", work);
  git("config commit.gpgsign false", work);

  writeFileSync(join(work, "a.txt"), "a\n");
  git("add a.txt", work);
  git("commit -m base", work);

  // Annotated tag with a multi-line message.
  git('tag -a v1.0.0 -m "Release v1.0.0" -m "Changelog: added feature"', work);
  // Lightweight tag (plain ref, no tag object).
  git("tag v1.0.1", work);

  repo = Repository.open(work);
});

afterAll(() => {
  rmSync(fixture, { recursive: true, force: true });
});

describe("tag listing", () => {
  test("tagNames / listTags cover annotated + lightweight", () => {
    const names = repo.tagNames();
    expect(names).toContain("v1.0.0");
    expect(names).toContain("v1.0.1");

    const infos: TagInfo[] = repo.listTags();
    expect(infos.length).toBe(2);

    const annotated = infos.find((t) => t.name === "v1.0.0")!;
    expect(annotated.kind).toBe(TagKind.Annotated);
    expect(annotated.message).not.toBeNull();
    expect(annotated.message!.startsWith("Release v1.0.0")).toBe(true);
    expect(annotated.tagger).not.toBeNull();
    // New global Signature.date field is RFC 3339.
    const tagger = annotated.tagger!;
    expect(tagger.date).not.toBeNull();
    expect(RFC3339.test(tagger.date!)).toBe(true);

    const light = infos.find((t) => t.name === "v1.0.1")!;
    expect(light.kind).toBe(TagKind.Lightweight);
    expect(light.message).toBeNull();
    expect(light.tagger).toBeNull();
  });

  test("tagNames(pattern) filters by short or full name", () => {
    expect(repo.tagNames("v1.0.0")).toEqual(["v1.0.0"]);
    expect(repo.tagNames("v1.0").sort()).toEqual(["v1.0.0", "v1.0.1"]);
  });
});

describe("find / inspect tags", () => {
  test("findTagByName: subject + taggerDate on an annotated tag", () => {
    const tag: Tag = repo.findTagByName("v1.0.0");
    expect(tag.name()).toBe("v1.0.0");
    expect(tag.subject()).toBe("Release v1.0.0");
    expect(tag.message()).toContain("Release v1.0.0");

    const tagger = tag.tagger()!;
    expect(tagger.date).not.toBeNull();
    expect(RFC3339.test(tagger.date!)).toBe(true);
    // Convenience getter mirrors tagger().date.
    expect(RFC3339.test(tag.taggerDate()!)).toBe(true);
  });

  test("findTagByName throws for a lightweight tag (no tag object)", () => {
    expect(() => repo.findTagByName("v1.0.1")).toThrow();
  });

  test("findTagByPrefix resolves an abbreviated hash to the tag object", () => {
    const id = repo.findTagByName("v1.0.0").id();
    // The full id is a trivially-unique prefix.
    expect(repo.findTagByPrefix(id).id()).toBe(id);
    // A 12-char prefix should also be unambiguous in this tiny repo.
    const short = id.slice(0, 12);
    expect(repo.findTagByPrefix(short).id()).toBe(id);
  });

  test("findTagByPrefix throws for an unmatched prefix", () => {
    expect(() => repo.findTagByPrefix("deadbeef")).toThrow();
  });
});
