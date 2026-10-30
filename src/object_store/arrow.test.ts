import { execSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { ArrowObjectStore } from "../main.ts";

let base: string;

beforeAll(() => {
  base = mkdtempSync(join(tmpdir(), "arrow-store-"));
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
});

describe("ArrowObjectStore (src/object_store/arrow.rs)", () => {
  test("path and objectFormat report in-memory store", () => {
    const store = new ArrowObjectStore("memory");
    expect(store.path).toBe("arrow://");
    expect(store.objectHash).toBe("sha1");
    expect(store.objectFormat).toBe("sha1");
  });

  test("writeObject() computes correct id and object is readable in memory", () => {
    const store = new ArrowObjectStore("memory");
    const blob = Buffer.from("arrow-memory-blob\n");
    const id = store.writeObject("blob", blob);
    expect(id).toMatch(/^[0-9a-f]{40}$/);
    expect(store.contains(id)).toBe(true);
    const found = store.tryFind(id);
    expect(found).not.toBeNull();
    expect(found!.kind).toBe("blob");
    expect(Buffer.from(found!.data).equals(blob)).toBe(true);
  });

  test("writeObjectWithId() stores under explicit id", () => {
    const store = new ArrowObjectStore("memory");
    const blob = Buffer.from("arrow-explicit-id-blob\n");
    const id = store.writeObject("blob", blob);
    const again = store.writeObjectWithId("blob", blob, id);
    expect(again).toBe(id);
    expect(store.contains(id)).toBe(true);
  });

  test("reader() streams an in-memory object", () => {
    const store = new ArrowObjectStore("memory");
    const blob = Buffer.from("streamed-arrow-blob\n");
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
    const store = new ArrowObjectStore("memory");
    const blob = Buffer.from("arrow-writer-blob\n");
    const w = store.writer("blob", blob.length);
    w.write(blob.subarray(0, 6));
    w.write(blob.subarray(6));
    const id = w.finalize();
    expect(store.contains(id)).toBe(true);
    expect(Buffer.from(store.tryFind(id)!.data).equals(blob)).toBe(true);
  });

  test("works on local filesystem directory", () => {
    const dir = join(base, "local-arrow");
    const store = new ArrowObjectStore(dir);
    expect(store.path).toBe("arrow://");
    const blob = Buffer.from("local-arrow-blob\n");
    const id = store.writeObject("blob", blob);
    expect(store.contains(id)).toBe(true);
    const header = store.tryHeader(id);
    expect(header).not.toBeNull();
    expect(header!.kind).toBe("blob");
    expect(header!.size).toBe(blob.length);
  });

  test("SHA-256 variant reports its format and uses 64-hex ids", () => {
    const store = new ArrowObjectStore("memory", "sha256");
    expect(store.objectHash).toBe("sha256");
    expect(store.objectFormat).toBe("sha256");
    const blob = Buffer.from("sha256-arrow-blob\n");
    const id = store.writeObject("blob", blob);
    expect(id).toMatch(/^[0-9a-f]{64}$/);
    expect(store.contains(id)).toBe(true);
    const header = store.tryHeader(id);
    expect(header!.kind).toBe("blob");
    expect(header!.size).toBe(blob.length);
  });
});
