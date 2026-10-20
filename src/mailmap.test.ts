import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";

import { Mailmap, Repository } from "./main.ts";

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

describe("Mailmap in-memory construction (src/mailmap.rs)", () => {
  test("empty() creates a mailmap with no entries", () => {
    const mm = Mailmap.empty();
    expect(mm.len()).toBe(0);
    expect(mm.isEmpty).toBe(true);
    expect(mm.resolve("Anyone", "anyone@example.com")).toBe(null);
    expect(mm.entries()).toEqual([]);
  });

  test("fromBuffer() parses .mailmap-format text", () => {
    const mm = Mailmap.fromBuffer(
      "Jane Doe <jane@example.com> <bugs@example.com>\n" +
        "Renamed Person <renamed@example.com> Old Person <old@example.com>\n",
    );
    expect(mm.len()).toBe(2);
    expect(mm.isEmpty).toBe(false);
    const resolved = mm.resolve("Anyone", "bugs@example.com");
    expect(resolved!.name).toBe("Jane Doe");
    expect(resolved!.email).toBe("jane@example.com");
  });

  test("fromBuffer() skips malformed lines like git does", () => {
    const mm = Mailmap.fromBuffer(
      "Jane Doe <jane@example.com> <bugs@example.com>\n@@not a mailmap line@@\n",
    );
    expect(mm.len()).toBe(1);
  });

  test("entries() lists every mapping with its old/new identity", () => {
    const mm = repo.openMailmap()!;
    const entries = mm.entries();
    expect(entries.length).toBe(2);

    const jane = entries.find((e) => e.oldEmail === "bugs@example.com");
    expect(jane).toBeDefined();
    expect(jane!.newName).toBe("Jane Doe");
    expect(jane!.newEmail).toBe("jane@example.com");
    expect(jane!.oldName ?? null).toBe(null);

    const renamed = entries.find((e) => e.oldEmail === "old@example.com");
    expect(renamed!.newName).toBe("Renamed Person");
    expect(renamed!.newEmail).toBe("renamed@example.com");
    expect(renamed!.oldName).toBe("Old Person");
  });

  test("addEntry() adds email-only, name-constrained, and name-only rewrites", () => {
    const mm = Mailmap.empty();
    // Full rewrite matched by email alone.
    mm.addEntry("Jane Doe", "jane@example.com", null, "bugs@example.com");
    // Email rewrite constrained by the old name.
    mm.addEntry(null, "renamed@example.com", "Old Person", "old@example.com");
    // Name-only rewrite (email passes through unchanged).
    mm.addEntry("Just A Name", null, null, "nameonly@example.com");
    expect(mm.len()).toBe(3);
    expect(mm.isEmpty).toBe(false);

    const a = mm.resolve("Anyone", "bugs@example.com");
    expect(a!.name).toBe("Jane Doe");
    expect(a!.email).toBe("jane@example.com");

    const b = mm.resolve("Old Person", "old@example.com");
    expect(b!.email).toBe("renamed@example.com");
    // The name-constrained entry does not apply to a different old name.
    expect(mm.resolve("Someone Else", "old@example.com")).toBe(null);

    const c = mm.resolve("Whoever", "nameonly@example.com");
    expect(c!.name).toBe("Just A Name");
    expect(c!.email).toBe("nameonly@example.com");
  });

  test("addEntry() with neither realName nor realEmail is a no-op", () => {
    const mm = Mailmap.empty();
    mm.addEntry(null, null, null, "noop@example.com");
    expect(mm.len()).toBe(0);
    expect(mm.isEmpty).toBe(true);
  });

  test("resolveSignature() preserves the timestamp and falls back unchanged", () => {
    const mm = repo.openMailmap()!;

    const mapped = mm.resolveSignature({
      name: "Old Person",
      email: "old@example.com",
      time: 1700000000,
      timezoneOffset: 60,
    });
    expect(mapped.name).toBe("Renamed Person");
    expect(mapped.email).toBe("renamed@example.com");
    expect(mapped.time).toBe(1700000000);
    expect(mapped.timezoneOffset).toBe(60);

    const untouched = {
      name: "Nobody",
      email: "nobody@example.com",
      time: 42,
      timezoneOffset: -30,
    };
    expect(mm.resolveSignature(untouched)).toEqual(untouched);
  });

  test("empty() + openMailmapInto() merges repository sources into a built mailmap", () => {
    const mm = Mailmap.empty();
    repo.openMailmapInto(mm);
    expect(mm.len()).toBe(2);
    expect(mm.resolve("Anyone", "bugs@example.com")!.name).toBe("Jane Doe");
  });
});
