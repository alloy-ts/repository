import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execSync } from "node:child_process";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { ArrowObjectStore, MemoryObjectStore } from "../main.ts";

let base: string;

beforeAll(() => {
  base = mkdtempSync(join(tmpdir(), "arrow-store-"));
});

afterAll(() => {
  rmSync(base, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 200,
  });
});

describe("ArrowObjectStore (src/object_store/arrow.rs)", () => {
  test("memory mode: path/objectFormat report the in-memory store", () => {
    const store = new ArrowObjectStore("memory", "sha1");
    expect(store.path).toBe("memory");
    expect(store.objectHash).toBe("sha1");
    expect(store.objectFormat).toBe("sha1");
  });

  test("memory mode: writeObject() computes the correct id and the object is readable", () => {
    const store = new ArrowObjectStore("memory", "sha1");
    const blob = Buffer.from("arrow-blob\n");
    const id = store.writeObject("blob", blob);
    expect(id).toMatch(/^[0-9a-f]{40}$/);
    expect(store.contains(id)).toBe(true);
    const found = store.tryFind(id);
    expect(found).not.toBeNull();
    expect(found!.kind).toBe("blob");
    expect(Buffer.from(found!.data).equals(blob)).toBe(true);
    expect(store.size()).toBe(1);
    expect(store.entries()).toEqual([id]);
  });

  test("memory mode: writeObjectWithId() stores under an explicit id", () => {
    const store = new ArrowObjectStore("memory", "sha1");
    const blob = Buffer.from("arrow-explicit\n");
    const id = store.writeObject("blob", blob);
    const again = store.writeObjectWithId("blob", blob, id);
    expect(again).toBe(id);
    expect(store.size()).toBe(1);
  });

  test("memory mode: reader() streams an object", () => {
    const store = new ArrowObjectStore("memory", "sha1");
    const blob = Buffer.from("arrow-streamed\n");
    const id = store.writeObject("blob", blob);
    const reader = store.reader(id);
    expect(reader.kind).toBe("blob");
    expect(reader.size).toBe(blob.length);
    const chunks: Buffer[] = [];
    let chunk = reader.read(4);
    while (chunk.length > 0) {
      chunks.push(Buffer.from(chunk));
      chunk = reader.read(4);
    }
    expect(Buffer.concat(chunks).equals(blob)).toBe(true);
  });

  test("memory mode: writer() round-trips a blob", () => {
    const store = new ArrowObjectStore("memory", "sha1");
    const blob = Buffer.from("arrow-writer\n");
    const w = store.writer("blob", blob.length);
    w.write(blob.subarray(0, 6));
    w.write(blob.subarray(6));
    const id = w.finalize();
    expect(store.contains(id)).toBe(true);
    expect(Buffer.from(store.tryFind(id)!.data).equals(blob)).toBe(true);
  });

  test("memory mode: writer() rejects writes beyond the declared size and mismatched final size", () => {
    const store = new ArrowObjectStore("memory", "sha1");
    const w = store.writer("blob", 3);
    expect(() => w.write(Buffer.from("ab"))).not.toThrow();
    expect(() => w.write(Buffer.from("cde"))).toThrow(); // would exceed 3

    const w2 = store.writer("blob", 5);
    w2.write(Buffer.from("abc"));
    expect(() => w2.finalize()).toThrow(); // only 3 of 5 bytes
  });

  test("memory mode: objectPath() errors for in-memory stores", () => {
    const store = new ArrowObjectStore("memory", "sha1");
    expect(() => store.objectPath("abc123")).toThrow();
  });

  test("memory mode: lookup_prefix() resolves a unique short id", () => {
    const store = new ArrowObjectStore("memory", "sha1");
    const id = store.writeObject("blob", Buffer.from("arrow-prefix\n"));
    const lookup = store.lookupPrefix(id.slice(0, 7));
    expect(lookup.found).toBe(true);
    expect(lookup.ambiguous).toBe(false);
    expect(lookup.id).toBe(id);
  });

  test("local fs mode: writes to disk, objectPath resolves, path is the canonical root", () => {
    const dir = join(base, "fs");
    const store = new ArrowObjectStore(dir, "sha1");
    const blob = Buffer.from("arrow-fs-blob\n");
    const id = store.writeObject("blob", blob);
    // `path` is the canonicalized on-disk root, which always contains our dir name.
    expect(store.path).toContain("fs");
    const p = store.objectPath(id);
    expect(p).toContain(id.slice(0, 2)); // two-char fanout directory
    expect(existsSync(p)).toBe(true);
    expect(Buffer.from(store.tryFind(id)!.data).equals(blob)).toBe(true);
    // The writer path also hits disk.
    const w = store.writer("blob", blob.length);
    w.write(blob);
    expect(w.finalize()).toMatch(/^[0-9a-f]{40}$/);
  });

  test("SHA-256 variant reports its format and uses 64-hex ids", () => {
    const store = new ArrowObjectStore("memory", "sha256");
    expect(store.objectHash).toBe("sha256");
    expect(store.objectFormat).toBe("sha256");
    const blob = Buffer.from("arrow-sha256\n");
    const id = store.writeObject("blob", blob);
    expect(id).toMatch(/^[0-9a-f]{64}$/);
    expect(store.contains(id)).toBe(true);
    const header = store.tryHeader(id);
    expect(header!.kind).toBe("blob");
    expect(header!.size).toBe(blob.length);
  });

  test("importStream() ingests a pack produced by MemoryObjectStore", () => {
    const src = new MemoryObjectStore();
    const blob = Buffer.from("arrow-import\n");
    const id = src.writeObject("blob", blob);
    const pack = src.dump();

    const dst = new ArrowObjectStore(join(base, "import"), "sha1");
    expect(dst.size()).toBe(0);
    dst.importStream(pack);
    expect(dst.contains(id)).toBe(true);
    expect(Buffer.from(dst.tryFind(id)!.data).equals(blob)).toBe(true);
  });

  test("gitLoose mode: writes zlib-compressed git loose objects (byte-compatible with git)", () => {
    const dir = join(base, "gitloose");
    const store = new ArrowObjectStore(dir, "sha1", true);
    expect(store.gitLoose).toBe(true);
    const blob = Buffer.from("arrow-git-loose\n");
    const id = store.writeObject("blob", blob);

    // The id matches git's content hash (validated against `git hash-object` separately).
    expect(id).toMatch(/^[0-9a-f]{40}$/);
    // The on-disk file must be a real zlib stream (git loose-object format), not the raw bytes.
    const bytes = readFileSync(store.objectPath(id));
    expect(bytes[0]).toBe(0x78); // zlib header
    expect((bytes[1] & 0x01) === 0x01).toBe(true);

    // Round-trips through the same store regardless of the compression.
    expect(store.contains(id)).toBe(true);
    expect(Buffer.from(store.tryFind(id)!.data).equals(blob)).toBe(true);
    expect(store.tryHeader(id)!.size).toBe(blob.length);

  test("list() and listWithDelimiter() enumerate stored objects", () => {
    const store = new ArrowObjectStore("memory", "sha1");
    const id1 = store.writeObject("blob", Buffer.from("list-item-1\n"));
    const id2 = store.writeObject("blob", Buffer.from("list-item-2\n"));

    const items = store.list();
    expect(items.length).toBe(2);
    expect(items.map((i) => i.location)).toContain(id1);
    expect(items.map((i) => i.location)).toContain(id2);

    const res = store.listWithDelimiter();
    expect(res.objects.length).toBe(2);
    expect(res.commonPrefixes).toEqual([]);
  });

  // And a real `git` must be able to read it when placed under `.git/objects`.
    let gitReads = false;
    try {
      execSync("git --version");
      const repo = mkdtempSync(join(tmpdir(), "gitloose-repo-"));
      execSync("git init -q", { cwd: repo });
      // Re-mount on the new repo's objects dir so the fanout lands under .git/objects.
      const loose = new ArrowObjectStore(join(repo, ".git", "objects"), "sha1", true);
      const id2 = loose.writeObject("blob", blob);
      expect(id2).toBe(id);
      const out = execSync(`git cat-file -p ${id}`, { cwd: repo }).toString();
      gitReads = out === blob.toString();
    } catch {
      // `git` not available in this environment; the zlib + round-trip checks above suffice.
    }
    expect(gitReads).toBe(true);
  });
});
