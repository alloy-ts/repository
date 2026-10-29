import { execSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { MemoryObjectStore } from "../main.ts";

const realRoot = process.cwd();

let base: string;

beforeAll(() => {
  base = mkdtempSync(join(tmpdir(), "memory-store-"));
});

afterAll(() => {
  rmSync(base, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 200,
  });
});

describe("MemoryObjectStore (src/object_store/memory.rs)", () => {
  test("path and objectFormat report the in-memory store", () => {
    const store = new MemoryObjectStore();
    expect(store.path).toBe("memory");
    expect(store.objectHash).toBe("sha1");
    expect(store.objectFormat).toBe("sha1");
  });

  test("writeObject() computes the correct id and the object is readable", () => {
    const store = new MemoryObjectStore();
    const blob = Buffer.from("memory-blob\n");
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
    const store = new MemoryObjectStore();
    const blob = Buffer.from("explicit-id-blob\n");
    const id = store.writeObject("blob", blob);
    // Writing the same content under its real id is a no-op overwrite.
    const again = store.writeObjectWithId("blob", blob, id);
    expect(again).toBe(id);
    expect(store.size()).toBe(1);
  });

  test("reader() streams an in-memory object", () => {
    const store = new MemoryObjectStore();
    const blob = Buffer.from("streamed-memory-blob\n");
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
    const store = new MemoryObjectStore();
    const blob = Buffer.from("memory-writer-blob\n");
    const w = store.writer("blob", blob.length);
    w.write(blob.subarray(0, 6));
    w.write(blob.subarray(6));
    const id = w.finalize();
    expect(store.contains(id)).toBe(true);
    expect(Buffer.from(store.tryFind(id)!.data).equals(blob)).toBe(true);
  });

  test("writer() rejects writes beyond the declared size and mismatched final size", () => {
    const store = new MemoryObjectStore();
    const w = store.writer("blob", 3);
    expect(() => w.write(Buffer.from("ab"))).not.toThrow();
    expect(() => w.write(Buffer.from("cde"))).toThrow(); // would exceed 3

    const w2 = store.writer("blob", 5);
    w2.write(Buffer.from("abc"));
    expect(() => w2.finalize()).toThrow(); // only 3 of 5 bytes
  });

  test("objectPath() errors for in-memory stores", () => {
    const store = new MemoryObjectStore();
    expect(() => store.objectPath("abc123")).toThrow();
  });

  test("dump()/load() round-trip the whole store", () => {
    const store = new MemoryObjectStore();
    const a = store.writeObject("blob", Buffer.from("alpha\n"));
    const b = store.writeObject("blob", Buffer.from("beta\n"));
    const pack = store.dump();
    expect(Buffer.from(pack).length).toBeGreaterThan(0);

    const other = new MemoryObjectStore();
    expect(other.size()).toBe(0);
    other.load(pack);
    expect(other.contains(a)).toBe(true);
    expect(other.contains(b)).toBe(true);
    expect(other.size()).toBe(2);
  });

  test("importStream() ingests a pack", () => {
    const src = new MemoryObjectStore();
    const blob = Buffer.from("import-memory-blob\n");
    const id = src.writeObject("blob", blob);
    const pack = src.dump();

    const dst = new MemoryObjectStore();
    dst.importStream(pack);
    expect(dst.contains(id)).toBe(true);
    expect(Buffer.from(dst.tryFind(id)!.data).equals(blob)).toBe(true);
  });

  test("reset() clears the store", () => {
    const store = new MemoryObjectStore();
    store.writeObject("blob", Buffer.from("temporary\n"));
    expect(store.size()).toBe(1);
    store.reset();
    expect(store.size()).toBe(0);
    expect(store.entries()).toEqual([]);
  });

  test("SHA-256 variant reports its format and uses 64-hex ids", () => {
    const store = new MemoryObjectStore({ objectHash: "sha256" });
    expect(store.objectHash).toBe("sha256");
    expect(store.objectFormat).toBe("sha256");
    const blob = Buffer.from("sha256-memory-blob\n");
    const id = store.writeObject("blob", blob);
    expect(id).toMatch(/^[0-9a-f]{64}$/);
    expect(store.contains(id)).toBe(true);
    const header = store.tryHeader(id);
    expect(header!.kind).toBe("blob");
    expect(header!.size).toBe(blob.length);
  });
});
