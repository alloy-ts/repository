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
  base = mkdtempSync(join(tmpdir(), "repo-notes-"));
  cloneDir = join(base, "clone");
  try {
    repo = await Repository.clone(realRoot, cloneDir, false);
  } catch {
    rmSync(cloneDir, { recursive: true, force: true });
    execSync(`git clone --local --quiet ${JSON.stringify(realRoot)} ${JSON.stringify(cloneDir)}`, {
      stdio: "ignore",
    });
    repo = Repository.open(cloneDir);
  }
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true });
});

describe("Repository notes (src/notes.rs)", () => {
  test("notesDefaultRef() is a string or null", () => {
    const d = repo.notesDefaultRef();
    expect(d === null || typeof d === "string").toBe(true);
  });

  test("set_note / notes_for / remove_note round-trip on the head commit", () => {
    const target = repo.headId();

    // no note initially
    expect(repo.notesFor(target)).toEqual([]);

    // add a note using the `commits` shorthand (refs/notes/commits)
    const previous = repo.setNote("commits", target, "review: looks good");
    expect(previous).toBe(null);

    const notes = repo.notesFor(target);
    expect(notes).toHaveLength(1);
    expect(notes[0]?.reference).toBe("refs/notes/commits");
    expect(notes[0]?.content).toBe("review: looks good");
    expect(notes[0]?.blobId).toMatch(/^[0-9a-f]{40}$/);

    // replace it; previous id is returned
    const replaced = repo.setNote("commits", target, "review: approved");
    expect(replaced).toMatch(/^[0-9a-f]{40}$/);
    expect(repo.notesFor(target)[0]?.content).toBe("review: approved");

    // remove it
    const removed = repo.removeNote("commits", target);
    expect(removed).toMatch(/^[0-9a-f]{40}$/);
    expect(repo.notesFor(target)).toEqual([]);

    // removing again is a no-op returning null
    expect(repo.removeNote("commits", target)).toBe(null);
  });

  test("set_note accepts a fully qualified notes ref and notes_in reads it", () => {
    const target = repo.headId();
    repo.setNote("refs/notes/review", target, "flagged");
    // the custom ref is not in the default selection...
    expect(repo.notesFor(target).some((n) => n.reference === "refs/notes/review")).toBe(false);
    // ...but notes_in can target it explicitly
    const notes = repo.notesIn(["refs/notes/review"], target);
    expect(notes).toHaveLength(1);
    expect(notes[0]?.reference).toBe("refs/notes/review");
    expect(notes[0]?.content).toBe("flagged");
    repo.removeNote("refs/notes/review", target);
    expect(repo.notesIn(["refs/notes/review"], target)).toEqual([]);
  });
});
