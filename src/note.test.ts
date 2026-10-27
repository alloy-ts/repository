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
    throw new Error("clone failed");
  }
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true });
});

describe("Repository notes (src/notes.rs)", () => {
  test("notesDefaultRef() always returns a string", () => {
    expect(typeof repo.notesDefaultRef()).toBe("string");
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
    expect(notes[0].reference).toBe("refs/notes/commits");
    expect(notes[0].content).toBe("review: looks good");
    expect(notes[0].blobId).toMatch(/^[0-9a-f]{40}$/);
    // contentBytes mirrors content as a byte array (napi serializes object byte
    // fields as number[]; Buffer.from reconstructs the text)
    expect(Array.isArray(notes[0].contentBytes)).toBe(true);
    expect(Buffer.from(notes[0].contentBytes as number[]).toString("utf8")).toBe(
      "review: looks good",
    );

    // replace it; previous id is returned
    const replaced = repo.setNote("commits", target, "review: approved");
    expect(replaced).toMatch(/^[0-9a-f]{40}$/);
    expect(repo.notesFor(target)[0].content).toBe("review: approved");

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
    expect(notes[0].reference).toBe("refs/notes/review");
    expect(notes[0].content).toBe("flagged");
    repo.removeNote("refs/notes/review", target);
    expect(repo.notesIn(["refs/notes/review"], target)).toEqual([]);
  });

  test("notes carry author/committer after a note commit exists", () => {
    const target = repo.headId();
    repo.setNote("commits", target, "with author");
    const note = repo.notesFor(target)[0];
    // The notes ref now points at a commit; git2's Note.author()/committer() map to it.
    if (note.author) {
      expect(typeof note.author.name).toBe("string");
      expect(typeof note.author.email).toBe("string");
    }
    repo.removeNote("commits", target);
  });

  test("find_note accepts an explicit notes_ref", () => {
    const target = repo.headId();
    repo.setNote("commits", target, "findable");
    const found = repo.findNote(target);
    expect(found).not.toBe(null);
    expect(found!.message).toBe("findable");
    // explicit ref yields the same note
    const foundExplicit = repo.findNote(target, "refs/notes/commits");
    expect(foundExplicit!.id).toBe(found!.id);
    // a ref with no note returns null
    expect(repo.findNote(target, "refs/notes/nonexistent")).toBe(null);
    repo.removeNote("commits", target);
  });

  test("force:false refuses to overwrite an existing note", () => {
    const target = repo.headId();
    repo.setNote("commits", target, "first");
    expect(() => repo.setNote("commits", target, "second", { force: false })).toThrow();
    // default (force:true) overwrites
    const prev = repo.setNote("commits", target, "second");
    expect(prev).toMatch(/^[0-9a-f]{40}$/);
    expect(repo.notesFor(target)[0].content).toBe("second");
    repo.removeNote("commits", target);
  });

  test("custom commit message is accepted", () => {
    const target = repo.headId();
    expect(() =>
      repo.setNote("commits", target, "msg", {
        commitMessage: "note: custom message",
      }),
    ).not.toThrow();
    repo.removeNote("commits", target);
  });

  test("list_notes enumerates every note in a ref", () => {
    const target = repo.headId();
    repo.setNote("commits", target, "listed");
    const all = repo.listNotes();
    const entry = all.find((n) => n.annotatedId === target);
    expect(entry).toBeTruthy();
    expect(entry!.content).toBe("listed");
    expect(entry!.reference).toBe("refs/notes/commits");
    expect(entry!.blobId).toMatch(/^[0-9a-f]{40}$/);
    // removal empties the list
    repo.removeNote("commits", target);
    expect(repo.listNotes().find((n) => n.annotatedId === target)).toBeFalsy();
  });

  test("notesRefs() returns an array of configured notes references", () => {
    const refs = repo.notesRefs();
    expect(Array.isArray(refs)).toBe(true);
  });

  test("sync and async note methods agree", async () => {
    const target = repo.headId();

    // read methods: identical results
    const sync = repo.notesFor(target);
    const async = await repo.notesForAsync(target);
    expect(async).toEqual(sync);

    const sl = repo.listNotes();
    const al = await repo.listNotesAsync();
    expect(al).toEqual(sl);

    const sf = repo.findNote(target);
    const af = await repo.findNoteAsync(target);
    expect(af).toEqual(sf);

    // a note written synchronously is readable asynchronously (and vice versa)
    const previous = repo.setNote("commits", target, "async");
    expect(previous).toBe(null);
    const viaAsync = await repo.notesForAsync(target);
    expect(viaAsync[0].content).toBe("async");

    const previous2 = await repo.removeNoteAsync("commits", target);
    expect(previous2).toMatch(/^[0-9a-f]{40}$/);
    expect(repo.notesFor(target)).toEqual([]);
  });
});
