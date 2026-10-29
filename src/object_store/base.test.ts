import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { LooseObjectStore, MemoryObjectStore } from "../main.ts";

let base: string;
beforeAll(() => {
  base = mkdtempSync(join(tmpdir(), "object-store-base-"));
});
afterAll(() => {
  rmSync(base, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
});

const blob = Buffer.from("object-store-base blob\n");

// Every lower-level store implements the shared `ObjectStore` base contract. Exercise both
// variants through the identical napi surface to prove the base is faithfully implemented.
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
