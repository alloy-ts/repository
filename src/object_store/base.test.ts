import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
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
    hasObjectPath: false,
  },
  {
    name: "MemoryObjectStore",
    make: () => new MemoryObjectStore(),
    hasObjectPath: false,
  },
  {
    name: "GenericObjectStore",
    make: () => new GenericObjectStore(join(base, "generic"), "sha1"),
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
    const id = w.finalise();
    expect(store.contains(id)).toBe(true);
    expect(Buffer.from(store.tryFind(id)!.data).equals(blob)).toBe(true);

    const tooBig = store.writer("blob", 3);
    expect(() => tooBig.write(Buffer.from("toolong"))).toThrow();
    const tooShort = store.writer("blob", 5);
    tooShort.write(Buffer.from("abc"));
    expect(() => tooShort.finalise()).toThrow();
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

  test("objectPath() errors for stores without per-object loose files", () => {
    const store = v.make();
    expect(() => store.objectPath("abc")).toThrow();
  });
});

// GenericObjectStore-specific behaviour that goes beyond the shared base contract: the local-fs
// (prolly node-store) backend, the SHA-256 variant, and the richer persist/fork surface. The store
// is implemented in src/object_store/base.rs as a copy-on-write prolly tree over a FileNodeStore, so
// it has no per-object loose file and `objectPath()` always errors. (This is the shared,
// external-crate-free implementation that `LooseObjectStore` and others wrap.)
describe("GenericObjectStore (src/object_store/base.rs)", () => {
  test("path/objectFormat report the on-disk node-store root", () => {
    const dir = join(base, "node-store");
    const store = new GenericObjectStore(dir, "sha1");
    expect(store.path).toBe(dir);
    expect(store.objectHash).toBe("sha1");
    expect(store.objectFormat).toBe("sha1");
  });

  test("writeObject() computes the correct id and the object is readable", () => {
    const store = new GenericObjectStore(join(base, "g1"), "sha1");
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

  test("writeObjectWithId() stores under an explicit id", () => {
    const store = new GenericObjectStore(join(base, "g2"), "sha1");
    const blob = Buffer.from("generic-explicit\n");
    const id = store.writeObject("blob", blob);
    const again = store.writeObjectWithId("blob", blob, id);
    expect(again).toBe(id);
    expect(store.size()).toBe(1);
  });

  test("reader() streams an object", () => {
    const store = new GenericObjectStore(join(base, "g3"), "sha1");
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

  test("writer() round-trips a blob", () => {
    const store = new GenericObjectStore(join(base, "g4"), "sha1");
    const blob = Buffer.from("generic-writer\n");
    const w = store.writer("blob", blob.length);
    w.write(blob.subarray(0, 6));
    w.write(blob.subarray(6));
    const id = w.finalise();
    expect(store.contains(id)).toBe(true);
    expect(Buffer.from(store.tryFind(id)!.data).equals(blob)).toBe(true);
  });

  test("writer() rejects writes beyond the declared size and mismatched final size", () => {
    const store = new GenericObjectStore(join(base, "g5"), "sha1");
    const w = store.writer("blob", 3);
    expect(() => w.write(Buffer.from("ab"))).not.toThrow();
    expect(() => w.write(Buffer.from("cde"))).toThrow(); // would exceed 3

    const w2 = store.writer("blob", 5);
    w2.write(Buffer.from("abc"));
    expect(() => w2.finalise()).toThrow(); // only 3 of 5 bytes
  });

  test("objectPath() errors: the prolly-backed store has no per-object loose file", () => {
    const store = new GenericObjectStore(join(base, "g6"), "sha1");
    expect(() => store.objectPath("abc123")).toThrow();
  });

  test("lookupPrefix() resolves a unique short id", () => {
    const store = new GenericObjectStore(join(base, "g7"), "sha1");
    const id = store.writeObject("blob", Buffer.from("generic-prefix\n"));
    const lookup = store.lookupPrefix(id.slice(0, 7));
    expect(lookup.found).toBe(true);
    expect(lookup.ambiguous).toBe(false);
    expect(lookup.id).toBe(id);
  });

  test("local fs backend: writes persist across reopening the node-store dir", () => {
    const dir = join(base, "fs");
    const store = new GenericObjectStore(dir, "sha1");
    const blob = Buffer.from("generic-fs-blob\n");
    const id = store.writeObject("blob", blob);
    // `path` is the on-disk node-store root.
    expect(store.path).toBe(dir);
    // There is no per-object loose file: `objectPath` always errors.
    expect(() => store.objectPath(id)).toThrow();
    // The content survives a fresh open of the same directory (prolly persistence).
    const reopened = new GenericObjectStore(dir, "sha1");
    expect(reopened.contains(id)).toBe(true);
    expect(Buffer.from(reopened.tryFind(id)!.data).equals(blob)).toBe(true);
    // The node-store directory exists on local fs.
    expect(existsSync(dir)).toBe(true);
    // The writer path also persists.
    const w = store.writer("blob", blob.length);
    w.write(blob);
    expect(w.finalise()).toMatch(/^[0-9a-f]{40}$/);
  });

  test("SHA-256 variant reports its format and uses 64-hex ids", () => {
    const store = new GenericObjectStore(join(base, "g8"), "sha256");
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

  test("fork() produces an independent copy that shares the node store until mutated", () => {
    const store = new GenericObjectStore(join(base, "gfork"), "sha1");
    const a = store.writeObject("blob", Buffer.from("alpha\n"));
    const b = store.writeObject("blob", Buffer.from("beta\n"));
    expect(store.size()).toBe(2);

    const forked = store.fork();
    expect(forked.size()).toBe(2);
    expect(forked.contains(a)).toBe(true);

    forked.reset();
    expect(forked.size()).toBe(0);
    expect(store.size()).toBe(2);
    expect(store.contains(a)).toBe(true);

    store.writeObject("blob", Buffer.from("gamma\n"));
    expect(store.size()).toBe(3);
    expect(forked.size()).toBe(0);
  });

  test("saveRoot()/restoreRoot() persist and reload the tree root", () => {
    const dir = join(base, "gsave");
    const store = new GenericObjectStore(dir, "sha1");
    const id = store.writeObject("blob", Buffer.from("persisted\n"));
    store.saveRoot();

    const reopened = new GenericObjectStore(dir, "sha1");
    expect(reopened.size()).toBe(1);
    expect(reopened.contains(id)).toBe(true);
    expect(Buffer.from(reopened.tryFind(id)!.data).equals(Buffer.from("persisted\n"))).toBe(true);
  });

  test("restoreRoot() throws when no root has been saved", () => {
    const store = new GenericObjectStore(join(base, "gnosave"), "sha1");
    expect(() => store.restoreRoot()).toThrow();
  });

  test("a store without saveRoot() does not see earlier contents on reopen", () => {
    const dir = join(base, "gunsaved");
    const store = new GenericObjectStore(dir, "sha1");
    store.writeObject("blob", Buffer.from("not-persisted\n"));
    const reopened = new GenericObjectStore(dir, "sha1");
    expect(reopened.size()).toBe(0);
  });
});
