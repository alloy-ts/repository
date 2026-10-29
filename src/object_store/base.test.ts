import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execSync } from "node:child_process";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { GenericObjectStore, LooseObjectStore, MemoryObjectStore } from "../main.ts";

let base: string;
beforeAll(() => {
  base = mkdtempSync(join(tmpdir(), "object-store-base-"));
});
afterAll(() => {
  rmSync(base, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 200,
  });
});

const blob = Buffer.from("object-store-base blob\n");

// Every lower-level store implements the shared `ObjectStore` base contract. Exercise each variant
// through the identical napi surface to prove the base is faithfully implemented.
const variants = [
  {
    name: "LooseObjectStore",
    make: () => new LooseObjectStore(join(base, "loose")),
    hasObjectPath: true,
  },
  {
    name: "MemoryObjectStore",
    make: () => new MemoryObjectStore(),
    hasObjectPath: false,
  },
  {
    name: "GenericObjectStore (memory)",
    make: () => new GenericObjectStore("memory", "sha1"),
    hasObjectPath: false,
  },
];

describe.each(variants)("ObjectStore shared base contract: $name", (v) => {
  test("writeObject() computes the id and the object is readable", () => {
    const store = v.make();
    const id = store.writeObject("blob", blob);
    expect(id).toMatch(/^[0-9a-f]{40}$/);
    expect(store.contains(id)).toBe(true);

    const found = store.tryFind(id);
    expect(found).not.toBeNull();
    expect(found!.kind).toBe("blob");
    expect(Buffer.from(found!.data).equals(blob)).toBe(true);

    const header = store.tryHeader(id);
    expect(header).not.toBeNull();
    expect(header!.kind).toBe("blob");
    expect(header!.size).toBe(blob.length);
  });

  test("missing objects return null / false (not errors)", () => {
    const store = v.make();
    const missing = "0000000000000000000000000000000000000000";
    expect(store.contains(missing)).toBe(false);
    expect(store.tryFind(missing)).toBeNull();
    expect(store.tryHeader(missing)).toBeNull();
  });

  test("lookupPrefix() resolves a written id", () => {
    const store = v.make();
    const id = store.writeObject("blob", blob);
    const lookup = store.lookupPrefix(id);
    expect(lookup.found).toBe(true);
    expect(lookup.ambiguous).toBe(false);
    expect(lookup.id).toBe(id);
  });

  test("reader() streams the object content in chunks", () => {
    const store = v.make();
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

  test("writer() round-trips a blob and enforces the declared size", () => {
    const store = v.make();
    const w = store.writer("blob", blob.length);
    w.write(blob);
    const id = w.finalize();
    expect(store.contains(id)).toBe(true);
    expect(Buffer.from(store.tryFind(id)!.data).equals(blob)).toBe(true);

    const tooBig = store.writer("blob", 3);
    expect(() => tooBig.write(Buffer.from("toolong"))).toThrow();
    const tooShort = store.writer("blob", 5);
    tooShort.write(Buffer.from("abc"));
    expect(() => tooShort.finalize()).toThrow();
  });

  test("writeObjectWithId() stores under an explicit id", () => {
    const store = v.make();
    const id = store.writeObject("blob", blob);
    const again = store.writeObjectWithId("blob", blob, id);
    expect(again).toBe(id);
  });

  test("objectHash / objectFormat report sha1 by default", () => {
    const store = v.make();
    expect(store.objectHash).toBe("sha1");
    expect(store.objectFormat).toBe("sha1");
  });

  test("importStream() ingests a pack produced by MemoryObjectStore.dump()", () => {
    const src = new MemoryObjectStore();
    const a = src.writeObject("blob", Buffer.from("import-a\n"));
    const b = src.writeObject("blob", Buffer.from("import-b\n"));
    const pack = src.dump();

    const store = v.make();
    store.importStream(pack);
    expect(store.contains(a)).toBe(true);
    expect(store.contains(b)).toBe(true);
  });

  if (v.hasObjectPath) {
    test("objectPath() returns the loose object file path", () => {
      const store = v.make();
      const id = store.writeObject("blob", blob);
      const p = store.objectPath(id);
      expect(typeof p).toBe("string");
      expect(p.length).toBeGreaterThan(0);
    });
  } else {
    test("objectPath() errors for in-memory stores", () => {
      const store = v.make();
      expect(() => store.objectPath("abc")).toThrow();
    });
  }
});

// GenericObjectStore-specific behaviour that goes beyond the shared base contract: the local-fs
// backend, the SHA-256 variant, git-loose byte compatibility, and the richer list/delete surface.
// (The store is implemented in src/object_store/base.rs.)
describe("GenericObjectStore (src/object_store/base.rs)", () => {
  test("memory mode: path/objectFormat report the in-memory store", () => {
    const store = new GenericObjectStore("memory", "sha1");
    expect(store.path).toBe("memory");
    expect(store.objectHash).toBe("sha1");
    expect(store.objectFormat).toBe("sha1");
  });

  test("memory mode: writeObject() computes the correct id and the object is readable", () => {
    const store = new GenericObjectStore("memory", "sha1");
    const blob = Buffer.from("generic-blob\n");
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
    const store = new GenericObjectStore("memory", "sha1");
    const blob = Buffer.from("generic-explicit\n");
    const id = store.writeObject("blob", blob);
    const again = store.writeObjectWithId("blob", blob, id);
    expect(again).toBe(id);
    expect(store.size()).toBe(1);
  });

  test("memory mode: reader() streams an object", () => {
    const store = new GenericObjectStore("memory", "sha1");
    const blob = Buffer.from("generic-streamed\n");
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
    const store = new GenericObjectStore("memory", "sha1");
    const blob = Buffer.from("generic-writer\n");
    const w = store.writer("blob", blob.length);
    w.write(blob.subarray(0, 6));
    w.write(blob.subarray(6));
    const id = w.finalize();
    expect(store.contains(id)).toBe(true);
    expect(Buffer.from(store.tryFind(id)!.data).equals(blob)).toBe(true);
  });

  test("memory mode: writer() rejects writes beyond the declared size and mismatched final size", () => {
    const store = new GenericObjectStore("memory", "sha1");
    const w = store.writer("blob", 3);
    expect(() => w.write(Buffer.from("ab"))).not.toThrow();
    expect(() => w.write(Buffer.from("cde"))).toThrow(); // would exceed 3

    const w2 = store.writer("blob", 5);
    w2.write(Buffer.from("abc"));
    expect(() => w2.finalize()).toThrow(); // only 3 of 5 bytes
  });

  test("memory mode: objectPath() errors for in-memory stores", () => {
    const store = new GenericObjectStore("memory", "sha1");
    expect(() => store.objectPath("abc123")).toThrow();
  });

  test("memory mode: lookup_prefix() resolves a unique short id", () => {
    const store = new GenericObjectStore("memory", "sha1");
    const id = store.writeObject("blob", Buffer.from("generic-prefix\n"));
    const lookup = store.lookupPrefix(id.slice(0, 7));
    expect(lookup.found).toBe(true);
    expect(lookup.ambiguous).toBe(false);
    expect(lookup.id).toBe(id);
  });

  test("local fs mode: writes to disk, objectPath resolves, path is the canonical root", () => {
    const dir = join(base, "fs");
    const store = new GenericObjectStore(dir, "sha1");
    const blob = Buffer.from("generic-fs-blob\n");
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
    const store = new GenericObjectStore("memory", "sha256");
    expect(store.objectHash).toBe("sha256");
    expect(store.objectFormat).toBe("sha256");
    const blob = Buffer.from("generic-sha256\n");
    const id = store.writeObject("blob", blob);
    expect(id).toMatch(/^[0-9a-f]{64}$/);
    expect(store.contains(id)).toBe(true);
    const header = store.tryHeader(id);
    expect(header!.kind).toBe("blob");
    expect(header!.size).toBe(blob.length);
  });

  test("importStream() ingests a pack produced by MemoryObjectStore", () => {
    const src = new MemoryObjectStore();
    const blob = Buffer.from("generic-import\n");
    const id = src.writeObject("blob", blob);
    const pack = src.dump();

    const dst = new GenericObjectStore(join(base, "import"), "sha1");
    expect(dst.size()).toBe(0);
    dst.importStream(pack);
    expect(dst.contains(id)).toBe(true);
    expect(Buffer.from(dst.tryFind(id)!.data).equals(blob)).toBe(true);
  });

  test("gitLoose mode: writes zlib-compressed git loose objects (byte-compatible with git)", () => {
    const dir = join(base, "gitloose");
    const store = new GenericObjectStore(dir, "sha1", true);
    expect(store.gitLoose).toBe(true);
    const blob = Buffer.from("generic-git-loose\n");
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

    // And a real `git` must be able to read it when placed under `.git/objects`.
    let gitReads = false;
    try {
      execSync("git --version");
      const repo = mkdtempSync(join(tmpdir(), "gitloose-repo-"));
      execSync("git init -q", { cwd: repo });
      // Re-mount on the new repo's objects dir so the fanout lands under .git/objects.
      const loose = new GenericObjectStore(join(repo, ".git", "objects"), "sha1", true);
      const id2 = loose.writeObject("blob", blob);
      expect(id2).toBe(id);
      const out = execSync(`git cat-file -p ${id}`, { cwd: repo }).toString();
      gitReads = out === blob.toString();
    } catch {
      // `git` not available in this environment; the zlib + round-trip checks above suffice.
    }
    expect(gitReads).toBe(true);
  });

  test("list() returns every object with id, location, size and lastModified", () => {
    const store = new GenericObjectStore(join(base, "list"), "sha1");
    const a = store.writeObject("blob", Buffer.from("list-a\n"));
    const b = store.writeObject("blob", Buffer.from("list-bb\n"));
    const items = store.list();
    expect(items.map((i) => i.id).sort()).toEqual([a, b].sort());
    const item = items.find((i) => i.id === a)!;
    expect(item.size).toBe(7); // "list-a\n"
    expect(item.location).toContain(a.slice(0, 2) + "/"); // fanout directory
    expect(item.lastModified).toBeGreaterThan(0);
  });

  test("list_with_delimiter() groups objects under their two-char fanout directory", () => {
    const store = new GenericObjectStore(join(base, "list-delim"), "sha1");
    store.writeObject("blob", Buffer.from("x1\n"));
    store.writeObject("blob", Buffer.from("x22\n"));
    const res = store.list_with_delimiter();
    // Every object lives under a `ab/` fanout dir, so none appear at the top level…
    expect(res.objects.length).toBe(0);
    // …and one common prefix per distinct two-char prefix is reported.
    expect(res.commonPrefixes.length).toBeGreaterThan(0);
    for (const cp of res.commonPrefixes) {
      expect(cp.endsWith("/")).toBe(true);
      expect(cp.length).toBe(3); // "ab/"
    }
  });

  test("delete() removes an object from the store", () => {
    const store = new GenericObjectStore(join(base, "del"), "sha1");
    const id = store.writeObject("blob", Buffer.from("to-delete\n"));
    expect(store.contains(id)).toBe(true);
    expect(store.size()).toBe(1);
    store.delete(id);
    expect(store.contains(id)).toBe(false);
    expect(store.size()).toBe(0);
    expect(store.list().length).toBe(0);
  });
});
