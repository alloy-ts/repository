import assert from "node:assert";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Repository } from "../src/main.ts";

// Run in an isolated temp repository so the example has no side effects on any
// real repo. A note can annotate *any* git object, so we annotate a freshly
// written blob instead of needing to create a commit first.
async function main() {
  const base = mkdtempSync(join(tmpdir(), "example-notes-"));
  const repo = Repository.init(base, false);

  try {
    const target = repo.writeBlob(Buffer.from("subject under review\n"));

    // The effective default notes reference (git's implicit refs/notes/commits
    // when core.notesRef is unset).
    const def = repo.notesDefaultRef();
    assert.equal(typeof def, "string");
    console.log("notesDefaultRef():         ", def);

    // Write a note on the default ref. The "commits" shorthand expands to
    // refs/notes/commits and the ref is created if absent.
    const previous = repo.setNote("commits", target, "review: looks good");
    assert.equal(previous, null); // nothing existed before

    // Read it back.
    const notes = repo.notesFor(target);
    assert.equal(notes.length, 1);
    const note = notes[0];
    assert.equal(note.reference, "refs/notes/commits");
    assert.equal(note.content, "review: looks good");
    assert.equal(note.blobId.length, 40); // equals git2's Note.id()
    console.log("notesFor()[0]:            ", JSON.stringify(note));

    // contentBytes mirrors the text as raw bytes (napi serializes object byte
    // fields as number[]).
    assert.ok(Array.isArray(note.contentBytes));
    assert.equal(Buffer.from(note.contentBytes).toString("utf8"), "review: looks good");

    // author / committer come from the notes-reference commit created by setNote.
    console.log("note.author:              ", JSON.stringify(note.author));
    console.log("note.committer:          ", JSON.stringify(note.committer));

    // findNote returns the same note (with an explicit ref this time).
    const found = repo.findNote(target, "refs/notes/commits");
    assert.ok(found);
    assert.equal(found!.message, "review: looks good");
    assert.equal(found!.id, note.blobId);

    // listNotes enumerates every note in a ref (git2's Notes iterator).
    const all = repo.listNotes();
    assert.equal(all.length, 1);
    assert.equal(all[0].annotatedId, target);
    assert.equal(all[0].content, "review: looks good");
    console.log("listNotes():              ", JSON.stringify(all));

    // notesRefs() lists configured notes references in lookup order.
    console.log("notesRefs():              ", JSON.stringify(repo.notesRefs()));

    // A custom, fully qualified notes ref — scoped via notesIn.
    repo.setNote("refs/notes/review", target, "flagged");
    const review = repo.notesIn(["refs/notes/review"], target);
    assert.equal(review.length, 1);
    assert.equal(review[0].reference, "refs/notes/review");
    // It does not appear in the default selection.
    assert.equal(
      repo.notesFor(target).some((n) => n.reference === "refs/notes/review"),
      false,
    );

    // force:false refuses to overwrite an existing note.
    assert.throws(() => repo.setNote("commits", target, "x", { force: false }));
    // Default (force:true) overwrites and returns the previous blob id.
    const replaced = repo.setNote("commits", target, "review: approved");
    assert.equal(typeof replaced, "string");

    // A custom commit message is accepted on write.
    repo.setNote("commits", target, "msg", {
      commitMessage: "note: custom message",
    });

    // Async variants agree with their sync counterparts.
    const aNotes = await repo.notesForAsync(target);
    assert.deepEqual(aNotes, repo.notesFor(target));

    const aList = await repo.listNotesAsync();
    assert.deepEqual(aList, repo.listNotes());

    const aFound = await repo.findNoteAsync(target, "refs/notes/commits");
    assert.deepEqual(aFound, repo.findNote(target, "refs/notes/commits"));

    const aRemoved = await repo.removeNoteAsync("commits", target);
    assert.equal(typeof aRemoved, "string");
    assert.equal(repo.notesFor(target).length, 0);

    // Clean up the custom ref too.
    repo.removeNote("refs/notes/review", target);

    console.log("\nAll notes API example assertions passed.");
  } finally {
    rmSync(base, { recursive: true, force: true });
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
