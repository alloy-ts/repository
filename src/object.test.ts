import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import {
  ObjectType,
  Oid,
  oidFormat,
  oidFromBytes,
  oidIsValid,
  oidIsZero,
  oidToBytes,
  oidZero,
  Repository,
} from "./main.ts";

const realRoot = process.cwd();
let base: string;
let cloneDir: string;
let repo: Repository;

beforeAll(async () => {
  base = mkdtempSync(join(tmpdir(), "repo-object-"));
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
});

afterAll(() => {
  rmSync(base, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 200,
  });
});

describe("Repository objects", () => {
  test("findObject returns a kinded wrapper with downcasts", () => {
    const commit = repo.headCommit();
    const asCommit = repo.findObject(commit.id);
    expect(asCommit.kind).toBe("commit");
    expect(asCommit.asCommit()?.id).toBe(commit.id);
    expect(asCommit.asTree()).toBeNull();

    const tree = repo.findTree(commit.treeId);
    const asTree = repo.findObject(tree.id);
    expect(asTree.kind).toBe("tree");
    expect(asTree.asTree()?.id).toBe(tree.id);
    expect(asTree.asCommit()).toBeNull();
  });

  test("objectExists() reports existing and missing objects", () => {
    expect(repo.objectExists(repo.headId())).toBe(true);
    expect(repo.objectExists("0".repeat(40))).toBe(false);
  });
});

describe("Objects / ODB (write_object, edit_tree, headers, empty)", () => {
  test("write_object() writes a raw object and findHeader reads its size", () => {
    const data = Buffer.from("hello world\n");
    const id = repo.writeObject("Blob", data);
    expect(id).toMatch(/^[0-9a-f]{40}$/);
    const header = repo.findHeader(id);
    expect(header.kind).toBe("blob");
    expect(header.size).toBe(data.length);
  });

  test("writeObject() and readObject() round-trip raw data, and computeHash() matches", () => {
    const data = Buffer.from("raw roundtrip\n");
    const id = repo.writeObject("Blob", data);
    const obj = repo.readObject(id);
    expect(obj.kind).toBe("blob");
    expect(Buffer.from(obj.data).equals(data)).toBe(true);
    expect(repo.computeHash("Blob", data)).toBe(id);
  });

  test("GitObject.data() returns the raw object body", () => {
    const id = repo.writeBlob(Buffer.from("gitobject data\n"));
    const obj = repo.findObject(id);
    expect(obj.kind).toBe("blob");
    expect(obj.data().toString()).toBe("gitobject data\n");
  });

  test("tryFindHeader() returns null for a missing object", () => {
    expect(repo.tryFindHeader("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef")).toBe(null);
  });

  test("findHeader() reports whether the object is packed", () => {
    const id = repo.writeObject("Blob", Buffer.from("packed?\n"));
    const header = repo.findObject(id).findHeader();
    expect(header.kind).toBe("blob");
    expect(typeof header.isPacked).toBe("boolean");
    // A freshly written object is loose, so there are no deltas to report.
    if (!header.isPacked) expect(header.numDeltas ?? null).toBe(null);
  });

  test("object cache size helpers do not throw", () => {
    expect(typeof repo.computeObjectCacheSizeForTreeDiffs()).toBe("number");
    expect(() => repo.objectCacheSize(1024)).not.toThrow();
    expect(() => repo.objectCacheSizeIfUnset(2048)).not.toThrow();
    expect(() => repo.withObjectMemory()).not.toThrow();
  });
});

describe("Object peeling (src/object.rs)", () => {
  test("peelToKind() reaches each kind and 'any' stops at the first non-tag", () => {
    const head = repo.headCommit();
    const tagId = repo.writeTag({
      name: "obj-peel",
      target: head.id,
      message: "peel\n",
    });

    const tag = repo.findObject(tagId);
    expect(tag.peelToKind("Commit").id).toBe(head.id);
    expect(tag.peelToKind("Tree").id).toBe(head.treeId);
    expect(tag.peelToTag().id).toBe(tagId);
    // `any` (and peelToEnd) follow the tag chain but never peel a commit to its tree.
    expect(tag.peelToKind("Any").id).toBe(head.id);
    expect(tag.peelToEnd().id).toBe(head.id);

    const commit = repo.findObject(head.id);
    expect(commit.peelToKind("Tree").id).toBe(head.treeId);
    expect(commit.peelToEnd().id).toBe(head.id);

    expect(() => tag.peelToKind("bogus")).toThrow(/ObjectType/);
  });

  test("peelToBlob() follows a tag that points at a blob", () => {
    const blobId = repo.writeBlob(Buffer.from("peel me\n"));
    const tagId = repo.writeTag({
      name: "obj-blob-tag",
      target: blobId,
      targetKind: "blob",
      message: "blob tag\n",
    });
    expect(repo.findObject(tagId).peelToBlob().id).toBe(blobId);
  });

  test("shortId() abbreviates the id unambiguously", () => {
    const commit = repo.headCommit();
    const short = repo.findObject(commit.id).shortId();
    expect(short.length).toBeGreaterThanOrEqual(7);
    expect(short.length).toBeLessThanOrEqual(commit.id.length);
    expect(commit.id.startsWith(short)).toBe(true);
  });

  test("oid helpers validate, convert and classify ids", () => {
    const id = repo.headId();

    expect(oidIsValid(id)).toBe(true);
    expect(oidIsValid(id, "sha1")).toBe(true);
    expect(oidIsValid(id, "Sha1")).toBe(true);
    // Strict length: abbreviations and wrong-format ids are not valid oids.
    expect(oidIsValid(id.slice(0, 7))).toBe(false);
    expect(oidIsValid(id, "sha256")).toBe(false);
    expect(oidIsValid("not-hex")).toBe(false);
    expect(oidIsValid(id, "bogus")).toBe(false);

    expect(oidFormat(id)).toBe("sha1");
    expect(oidFormat("nope")).toBe(null);

    expect(oidIsZero(oidZero())).toBe(true);
    expect(oidIsZero(id)).toBe(false);
    expect(oidZero("sha256").length).toBe(64);

    const bytes = oidToBytes(id);
    expect(bytes.length).toBe(20);
    expect(oidFromBytes(bytes)).toBe(id);
    expect(() => oidFromBytes(Buffer.alloc(8))).toThrow();
  });

  test("Oid class wraps, converts and compares object ids", () => {
    const id = repo.headId();
    const oid = new Oid(id);

    expect(oid.hex).toBe(id);
    expect(String(oid)).toBe(id);
    expect(JSON.parse(JSON.stringify({ id: oid })).id).toBe(id);

    expect(oid.format()).toBe("sha1");
    expect(oid.isZero()).toBe(false);
    expect(oid.equals(new Oid(id))).toBe(true);
    expect(oid.equals(Oid.zero())).toBe(false);
    expect(oid.compare(new Oid(id))).toBe(0);
    expect(Oid.zero().compare(oid)).toBe(-1);
    expect(oid.compare(Oid.zero())).toBe(1);

    const bytes = oid.toBytes();
    expect(bytes.length).toBe(20);
    expect(Oid.fromBytes(bytes).equals(oid)).toBe(true);
    expect(() => Oid.fromBytes(Buffer.alloc(8))).toThrow();

    const zero = Oid.zero();
    expect(zero.hex).toBe("0".repeat(40));
    expect(zero.isZero()).toBe(true);
    expect(Oid.zero("Sha256").hex.length).toBe(64);
    expect(() => Oid.zero("bogus")).toThrow(/unknown object hash kind/);

    // Strict construction: abbreviations and non-hex input are not object ids.
    expect(() => new Oid(id.slice(0, 7))).toThrow();
    expect(() => new Oid("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz")).toThrow();
  });

  test("Oid.fromHex parses leniently, or strictly when a format is given", () => {
    const id = repo.headId();

    // Lenient: no format → identical to the constructor (auto-detects by length).
    expect(Oid.fromHex(id).equals(new Oid(id))).toBe(true);
    // Explicit matching format is accepted.
    expect(Oid.fromHex(id, "sha1").equals(new Oid(id))).toBe(true);
    // A valid SHA-256 id with its format.
    const sha256 = "0".repeat(64);
    expect(Oid.fromHex(sha256, "sha256").hex).toBe(sha256);

    // Format mismatch is rejected (a 40-hex SHA-1 string is not a SHA-256 id).
    expect(() => Oid.fromHex(id, "sha256")).toThrow(/does not match/);
    // Unknown format string is rejected.
    expect(() => Oid.fromHex(id, "bogus")).toThrow(/unknown object hash kind/);
  });

  test("computeHashFile() hashes a file without writing it", () => {
    const file = join(base, "hash-me.txt");
    writeFileSync(file, "hash file contents\n");
    const id = repo.computeHashFile("Blob", file);
    expect(id).toBe(repo.computeHash("Blob", Buffer.from("hash file contents\n")));
    expect(repo.objectExists(id)).toBe(false);
    expect(() => repo.computeHashFile("blob", join(base, "missing.txt"))).toThrow();
  });
});

describe("Object ergonomics (src/object.rs)", () => {
  test("size() matches the header size and the raw data length", () => {
    const data = Buffer.from("size me\n");
    const id = repo.writeBlob(data);
    const obj = repo.findObject(id);
    expect(obj.size).toBe(data.length);
    expect(obj.size).toBe(obj.findHeader().size);
    expect(obj.size).toBe(obj.data().length);
  });

  test("objectType() is the enum and agrees with kind()", () => {
    const commit = repo.headCommit();
    const c = repo.findObject(commit.id);
    expect(c.objectType).toBe(ObjectType.Commit);
    expect(c.kind).toBe("commit");

    const blobId = repo.writeBlob(Buffer.from("t\n"));
    const b = repo.findObject(blobId);
    expect(b.objectType).toBe(ObjectType.Blob);
    expect(b.kind).toBe("blob");

    const treeId = repo.headCommit().treeId;
    const t = repo.findObject(treeId);
    expect(t.objectType).toBe(ObjectType.Tree);
    expect(t.kind).toBe("tree");
  });

  test("content() mirrors readObject", () => {
    const data = Buffer.from("content body\n");
    const id = repo.writeBlob(data);
    const obj = repo.findObject(id);
    const content = obj.content();
    expect(content.kind).toBe("blob");
    expect(Buffer.from(content.data).equals(data)).toBe(true);
    // Equivalent to the repository-level readObject.
    const ro = repo.readObject(id);
    expect(Buffer.from(content.data).equals(Buffer.from(ro.data))).toBe(true);
  });

  test("text() decodes utf-8 by default and via utf16le", () => {
    const id = repo.writeBlob(Buffer.from("hello\n"));
    expect(repo.findObject(id).text()).toBe("hello\n");

    // UTF-16 LE bytes for "hi" => 68 00 69 00.
    const leId = repo.writeBlob(Buffer.from([0x68, 0x00, 0x69, 0x00]));
    expect(repo.findObject(leId).text("utf16le")).toBe("hi");
  });

  test("peel(kind) is an alias of peelToKind", () => {
    const head = repo.headCommit();
    const tagId = repo.writeTag({
      name: "ergo-peel",
      target: head.id,
      message: "p\n",
    });
    const tag = repo.findObject(tagId);
    expect(tag.peel("Commit").id).toBe(head.id);
    expect(tag.peel("Tree").id).toBe(head.treeId);
    expect(tag.peel("Any").id).toBe(head.id);
  });

  test("peelToObject(id, kind) returns the peeled Object", () => {
    const head = repo.headCommit();
    const tagId = repo.writeTag({
      name: "ergo-peel-obj",
      target: head.id,
      message: "p\n",
    });
    const peeled = repo.peelToObject(tagId, "Commit");
    expect(peeled.id).toBe(head.id);
    expect(peeled.kind).toBe("commit");
    // Equivalent to a follow-up findObject on the peeled id.
    expect(repo.findObject(peeled.id).id).toBe(peeled.id);
    expect(() => repo.peelToObject(tagId, "Any")).toThrow(/Any.*not supported/);
  });
});
