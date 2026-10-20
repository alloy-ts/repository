import { execSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository } from "./main.ts";

// Draft tests for the versioned-object Replace API.
//
// Model:
//   refs/objects/{origOid}    -> latest replacement content (replaceOid)
//   refs/objects/{replaceOid} -> this revision's content (self reference)
//   refs/replace/{replaceOid} -> a single `Revision` node
//                                { this, prev?, next?, head?, authorName,
//                                  authorEmail, updated_at, committerName?,
//                                  committerEmail?, commitTimestamp? }
//   refs/replace/{origOid}    -> the root `ReplaceManifest`
//                                { $ref, id, type, path?, head, revisions[] }
//   refs/replace/{entryPath}  -> alias to the same manifest (tracked files only)

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

function git(args: string, cwd: string) {
  execSync(`git ${args}`, { cwd, stdio: "ignore" });
}

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-replace-"));
  cloneDir = join(base, "clone");
  try {
    repo = await Repository.clone(realRoot, cloneDir);
  } catch {
    execSync(`git clone --local --quiet ${JSON.stringify(realRoot)} ${JSON.stringify(cloneDir)}`, {
      stdio: "ignore",
    });
    repo = Repository.open(cloneDir);
  }
  git("config commit.gpgsign false", cloneDir);
});

afterAll(() => {
  rmSync(base, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 200,
  });
});

describe("Repository replace / try_replace (draft)", () => {
  test("replace() records a Revision and catFile returns the latest content", () => {
    const id = repo.writeBlob(Buffer.from("original-contents"));
    const out = repo.replace(id, Buffer.from("replaced-contents"), {
      message: "v1",
    });

    expect(out.replacementId.length).toBe(40);
    expect(out.reference).toBe(`refs/replace/${id}`);
    expect(out.manifest.id).toBe(id);
    expect(out.manifest.revisions.length).toBe(1);
    expect(out.manifest.head).toBe(out.replacementId);

    // The first Revision node: `this` is the new content id, no `prev` yet.
    const rev = out.manifest.revisions[0];
    expect(rev.this).toBe(out.replacementId);
    expect(rev.prev ?? null).toBe(null);
    expect(rev.head).toBe(out.replacementId);
    expect(rev.authorName).toBeTypeOf("string");
    expect(rev.authorEmail).toBeTypeOf("string");
    expect(rev.updated_at).toBeTypeOf("string");

    // Reading the original oid yields the replacement content.
    expect((repo.catFile(id) as Buffer).toString("utf8")).toBe("replaced-contents");

    // getReference follows the replacement and returns the latest content id.
    expect(repo.getReference(id)).toBe(out.replacementId);
  });

  test("each revision is stored at refs/replace/{replaceOid} and links prev/next", () => {
    const id = repo.writeBlob(Buffer.from("v0"));

    const r1 = repo.replace(id, Buffer.from("v1"));
    const n1a = repo.getReplaceNode(r1.replacementId)!;
    expect(n1a.this).toBe(r1.replacementId);
    // First revision has no previous; `head` points at the current latest.
    expect(n1a.prev ?? null).toBe(null);
    expect(n1a.head).toBe(r1.replacementId);

    const r2 = repo.replace(id, Buffer.from("v2"), { force: true });

    // Re-read the first node: the new revision should have linked itself in.
    const n1b = repo.getReplaceNode(r1.replacementId)!;
    const n2 = repo.getReplaceNode(r2.replacementId)!;

    // Walk forward: n1.next points at r2; walk back: n2.prev points at r1.
    expect(n1b.next).toBe(r2.replacementId);
    expect(n2.prev).toBe(r1.replacementId);
    // `head` caches the latest revision for fast traversal.
    expect(n1b.head).toBe(r2.replacementId);
    expect(n2.head).toBe(r2.replacementId);
    expect(n2.next ?? null).toBe(null);
  });

  test("the manifest registers every revision node and tracks head", () => {
    const id = repo.writeBlob(Buffer.from("v0"));
    const r1 = repo.replace(id, Buffer.from("v1"));
    const r2 = repo.replace(id, Buffer.from("v2"), { force: true });
    const r3 = repo.replace(id, Buffer.from("v3"), { force: true });

    const m = r3.manifest;
    expect(m.id).toBe(id);
    expect(m.head).toBe(r3.replacementId);
    expect(m.revisions.map((v) => v.this)).toEqual([
      r1.replacementId,
      r2.replacementId,
      r3.replacementId,
    ]);
    // Linked-list integrity across the flat array.
    expect(m.revisions[0].prev ?? null).toBe(null);
    expect(m.revisions[1].prev).toBe(r1.replacementId);
    expect(m.revisions[2].prev).toBe(r2.replacementId);
    expect(m.revisions[2].next ?? null).toBe(null);
  });

  test("try_replace() returns null when a replace ref already exists", () => {
    const id = repo.writeBlob(Buffer.from("first-version"));
    repo.replace(id, Buffer.from("second-version"));
    expect(repo.try_replace(id, Buffer.from("third-version"))).toBe(null);
  });

  test("replace with a string content works the same as a Buffer", () => {
    const id = repo.writeBlob(Buffer.from("blob-A"));
    const out = repo.replace(id, "blob-B-string");
    expect((repo.catFile(id) as Buffer).toString("utf8")).toBe("blob-B-string");
    expect(out.replacementId.length).toBe(40);
  });

  test("GitObject.replace is the same API as Repository.replace (delegates)", () => {
    const id = repo.writeBlob(Buffer.from("obj-original"));
    // `find_object` returns the kinded `GitObject` wrapper, which carries a `replace`.
    const obj = repo.find_object(id);
    const out = obj.replace(Buffer.from("obj-replaced"), {
      message: "via-object",
    });
    expect(out?.replacementId.length).toBe(40);
    expect(out?.manifest.id).toBe(id);
    expect(out?.manifest.revisions.length).toBe(1);
    // Reading the original id follows the replacement.
    expect((repo.catFile(id) as Buffer).toString("utf8")).toBe("obj-replaced");
    // The revision node is individually walkable.
    const node = repo.getReplaceNode(out!.replacementId)!;
    expect(node.this).toBe(out?.replacementId);
    expect(node.authorName).toBeTypeOf("string");
  });

  test("Entry.replace delegates to the repository replace API", () => {
    const id = repo.writeBlob(Buffer.from("entry-original"));
    // Put the blob in a tree so it can be resolved as an `Entry`.
    const treeId = repo.writeTree([
      {
        path: "nested/file.txt",
        mode: "100644",
        id,
      },
    ]);
    const entry = repo.findTree(treeId).entryByPath("nested/file.txt")!;
    expect(entry).not.toBeNull();
    const out = entry.replace(Buffer.from("entry-replaced"), {
      message: "via-entry",
    });
    expect(out.replacementId.length).toBe(40);
    // Reading the original id follows the replacement.
    expect((repo.catFile(id) as Buffer).toString("utf8")).toBe("entry-replaced");
    // The revision node is individually walkable.
    const node = repo.getReplaceNode(out.replacementId)!;
    expect(node.this).toBe(out.replacementId);
  });
});

describe("replace per object type", () => {
  test("blob: replace records a blob revision and follows through", () => {
    const id = repo.writeBlob(Buffer.from("blob-original"));
    const out = repo.replace(id, Buffer.from("blob-replaced"), {
      message: "blob",
    });
    expect(out.manifest.type).toBe("blob");
    expect((repo.catFile(id) as Buffer).toString("utf8")).toBe("blob-replaced");
  });

  test("tree: replace records a tree revision with commit provenance", () => {
    const blobId = repo.writeBlob(Buffer.from("tree-blob"));
    const treeId = repo.writeTree([
      {
        path: "f.txt",
        mode: "100644",
        id: blobId,
      },
    ]);
    const out = repo.replace(treeId, repo.catFile(treeId) as Buffer, {
      message: "tree",
    });
    // The replacement object stays a tree (same type as the original).
    expect(out.manifest.type).toBe("tree");
    expect(out.manifest.revisions.length).toBe(1);
    // Trees live inside a commit, so the node carries the containing commit's provenance.
    const node = repo.getReplaceNode(out.replacementId)!;
    expect(node.commitTimestamp).toBeTypeOf("string");
    expect((node.committerName as string).length).toBeGreaterThan(0);
    expect((node.authorName as string).length).toBeGreaterThan(0);
  });

  test("commit: replace records a commit revision with its own provenance", () => {
    const commit = repo.headCommit();
    const bytes = repo.catFile(commit.id) as Buffer;
    const out = repo.replace(commit.id, bytes, { message: "commit" });
    // The replacement object stays a commit (same type as the original).
    expect(out.manifest.type).toBe("commit");
    // A commit carries its own author/committer/timestamp.
    const node = repo.getReplaceNode(out.replacementId)!;
    expect(node.authorName).toBeTypeOf("string");
    expect(node.committerName).toBeTypeOf("string");
    expect(node.commitTimestamp).toBeTypeOf("string");
    expect((node.commitTimestamp as string).length).toBeGreaterThan(0);
  });

  test("tag: replace records a tag revision", () => {
    const commit = repo.headCommit();
    const tagId = repo.writeTag({
      name: "v-test-replace",
      target: commit.id,
      target_kind: "commit",
      message: "tag for replace test",
    });
    const bytes = repo.catFile(tagId) as Buffer;
    const out = repo.replace(tagId, bytes, { message: "tag" });
    expect(out.manifest.type).toBe("tag");
  });
});

describe("entry-type replace carries commit provenance", () => {
  test("entry replace sets type 'entry' and commit metadata from the containing commit", () => {
    const treeId = repo.headTreeId();
    // Use a real tracked file so entry detection (committed in HEAD/default-branch tree) applies.
    const entry = repo.findTree(treeId).entryByPath("Cargo.toml")!;
    expect(entry).not.toBeNull();
    const out = entry.replace(Buffer.from("entry-replaced-body"), {
      force: true,
    });
    expect(out.manifest.type).toBe("entry");
    expect(out.manifest.path).toBe("Cargo.toml");

    // The individual node carries the author/committer provenance of the commit that
    // contains this entry (recorded at refs/replace/{replaceOid}).
    const node = repo.getReplaceNode(out.replacementId)!;
    expect(node.authorName).toBeTypeOf("string");
    expect(node.authorEmail).toBeTypeOf("string");
    expect(node.committerName).toBeTypeOf("string");
    expect(node.committerEmail).toBeTypeOf("string");
    expect(node.commitTimestamp).toBeTypeOf("string");
    expect((node.committerName as string).length).toBeGreaterThan(0);
    expect((node.commitTimestamp as string).length).toBeGreaterThan(0);
  });
});

describe("repeated replace of the same object accumulates revisions", () => {
  test("each replacement creates a new object merged into one manifest at refs/replace/{initOid}", () => {
    const id = repo.writeBlob(Buffer.from("v0"));
    const r1 = repo.replace(id, Buffer.from("v1"), { message: "r1" });
    const r2 = repo.replace(id, Buffer.from("v2"), { force: true });
    const r3 = repo.replace(id, Buffer.from("v3"), { force: true });

    // The single canonical manifest (refs/replace/{initOid}) holds every revision, and is
    // stable: its `id` is always the initial object oid, while `head` advances.
    const m = r3.manifest;
    expect(m.id).toBe(id);
    expect(m.head).toBe(r3.replacementId);
    expect(m.revisions.length).toBe(3);
    expect(m.revisions.map((v) => v.this)).toEqual([
      r1.replacementId,
      r2.replacementId,
      r3.replacementId,
    ]);
    // Each replacement is a distinct object.
    expect(new Set(m.revisions.map((v) => v.this)).size).toBe(3);

    // Reading the initial oid follows the replacement chain to the latest content, even
    // though refs/objects/{initOid} was overwritten to point at the newest replacement.
    expect((repo.catFile(id) as Buffer).toString("utf8")).toBe("v3");
    expect(repo.getReference(id)).toBe(r3.replacementId);
  });

  test("entry at a fixed path accumulates revisions like a versioned file", () => {
    const treeId = repo.headTreeId();
    const entry = repo.findTree(treeId).entryByPath("Cargo.toml")!;
    expect(entry).not.toBeNull();

    const r1 = entry.replace(Buffer.from("file-v1"), { force: true });
    const r2 = entry.replace(Buffer.from("file-v2"), { force: true });
    const r3 = entry.replace(Buffer.from("file-v3"), { force: true });

    // Same path/oid => one manifest (refs/replace/{initOid} and the path alias), revisions[] grows.
    expect(r3.manifest.type).toBe("entry");
    expect(r3.manifest.path).toBe("Cargo.toml");
    expect(r3.manifest.revisions.length).toBe(3);
    expect(r3.manifest.head).toBe(r3.replacementId);

    // The latest revision node still carries commit provenance.
    const node = repo.getReplaceNode(r3.replacementId)!;
    expect((node.committerName as string).length).toBeGreaterThan(0);
    expect((node.commitTimestamp as string).length).toBeGreaterThan(0);

    // Walking back from the head reaches every prior revision.
    const seen: string[] = [];
    let cursor: string | null = r3.replacementId;
    for (let i = 0; i < 3 && cursor; i++) {
      const n = repo.getReplaceNode(cursor)!;
      seen.push(n.this);
      cursor = n.prev ?? null;
    }
    expect(seen).toEqual([r3.replacementId, r2.replacementId, r1.replacementId]);
  });
});
