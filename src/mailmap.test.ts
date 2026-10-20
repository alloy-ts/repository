import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";

import { Repository } from "./main.ts";

let fixture: string;
let repo: Repository;

function git(args: string, cwd: string) {
  execSync(`git ${args}`, { cwd, stdio: "ignore" });
}

beforeAll(() => {
  fixture = mkdtempSync(join(tmpdir(), "repository-mailmap-"));
  git("init --initial-branch=main", fixture);
  git("config user.email test@example.com", fixture);
  git("config user.name Test", fixture);
  // The dev machine signs commits globally; the throwaway fixture has no gpg, so disable it.
  git("config commit.gpgsign false", fixture);

  // Standard `.mailmap` syntax (verified against gix-mailmap's own docs):
  //   "<New Name> <new@email> <old@email>"             → rewrite email (and name)
  //   "<New Name> <new@email> <Old Name> <old@email>"  → rewrite, matching name too
  const mailmap = [
    "Jane Doe <jane@example.com> <bugs@example.com>",
    "Renamed Person <renamed@example.com> Old Person <old@example.com>",
    "",
  ].join("\n");
  writeFileSync(join(fixture, ".mailmap"), mailmap);

  writeFileSync(join(fixture, "README.md"), "hello\n");
  git("add -A", fixture);
  git('commit -m "init with mailmap"', fixture);

  repo = Repository.open(fixture);
});

afterAll(() => {
  rmSync(fixture, { recursive: true, force: true });
});

describe("Repository mailmap (src/mailmap.rs)", () => {
  test("openMailmap() returns null when there is no mailmap", () => {
    // A fixture without a `.mailmap` file.
    const empty = mkdtempSync(join(tmpdir(), "repository-mailmap-empty-"));
    git("init --initial-branch=main", empty);
    git("config user.email test@example.com", empty);
    git("config user.name Test", empty);
    git("config commit.gpgsign false", empty);
    writeFileSync(join(empty, "f.txt"), "x\n");
    git("add -A", empty);
    git('commit -m "init"', empty);
    expect(Repository.open(empty).openMailmap()).toBe(null);
    rmSync(empty, { recursive: true, force: true });
  });

  test("openMailmap() loads the worktree .mailmap", () => {
    const mailmap = repo.openMailmap();
    expect(mailmap).not.toBeNull();
    // Two lines in the fixture's `.mailmap`.
    expect(mailmap!.len()).toBe(2);
  });

  test("resolve() rewrites an email-only mapping", () => {
    const mailmap = repo.openMailmap()!;
    // Only the email is matched for the first entry.
    const resolved = mailmap.resolve("Any Author Name", "bugs@example.com");
    expect(resolved).not.toBeNull();
    expect(resolved!.name).toBe("Jane Doe");
    expect(resolved!.email).toBe("jane@example.com");
  });

  test("resolve() honors the name-specific mapping", () => {
    const mailmap = repo.openMailmap()!;
    // The second entry only applies when both the old name and email match.
    const matched = mailmap.resolve("Old Person", "old@example.com");
    expect(matched!.name).toBe("Renamed Person");
    expect(matched!.email).toBe("renamed@example.com");

    // A different name with the same email does not match that entry.
    expect(mailmap.resolve("Someone Else", "old@example.com")).toBe(null);
  });

  test("resolve() returns null for an unmapped identity", () => {
    const mailmap = repo.openMailmap()!;
    expect(mailmap.resolve("Nobody", "nobody@example.com")).toBe(null);
  });

  test("openMailmapInto() merges additional sources into an existing snapshot", () => {
    const mailmap = repo.openMailmap()!;
    // Merging the same repository's mailmap is idempotent, so the size stays the same.
    repo.openMailmapInto(mailmap);
    expect(mailmap.len()).toBe(2);
  });
});
