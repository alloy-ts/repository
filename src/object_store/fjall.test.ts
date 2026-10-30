import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { FjallObjectStore } from "../main.ts";

let baseDir: string;

beforeAll(() => {
  baseDir = mkdtempSync(join(tmpdir(), "fjall-store-"));
});

afterAll(() => {
  rmSync(baseDir, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 200,
  });
});

describe("FjallObjectStore (src/object_store/fjall.rs)", () => {
  test("creates and operates on embedded LSM-tree store", () => {
    const dbPath = join(baseDir, "fjall-db-1");
    const store = new FjallObjectStore(dbPath);

    expect(store.path).toBe(dbPath);
    expect(store.objectHash).toBe("sha1");
    expect(store.objectFormat).toBe("sha1");

    const blob = Buffer.from("fjall-test-blob\n");
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

  test("lookupPrefix finds object by short prefix", () => {
    const dbPath = join(baseDir, "fjall-db-2");
    const store = new FjallObjectStore(dbPath);

    const blob = Buffer.from("prefix-test-content\n");
    const id = store.writeObject("blob", blob);

    const prefix = id.substring(0, 8);
    const lookup = store.lookupPrefix(prefix);
    expect(lookup.found).toBe(true);
    expect(lookup.ambiguous).toBe(false);
    expect(lookup.id).toBe(id);
  });

  test("reader streams content from Fjall store", () => {
    const dbPath = join(baseDir, "fjall-db-3");
    const store = new FjallObjectStore(dbPath);

    const blob = Buffer.from("reader-stream-content\n");
    const id = store.writeObject("blob", blob);

    const reader = store.reader(id);
    expect(reader.kind).toBe("blob");
    expect(reader.size).toBe(blob.length);

    const chunk = reader.read(blob.length);
    expect(Buffer.from(chunk).equals(blob)).toBe(true);
  });

  test("supports SHA-256 hash kind", () => {
    const dbPath = join(baseDir, "fjall-db-4");
    const store = new FjallObjectStore(dbPath, "sha256");

    expect(store.objectHash).toBe("sha256");
    expect(store.objectFormat).toBe("sha256");

    const blob = Buffer.from("sha256-content\n");
    const id = store.writeObject("blob", blob);
    expect(id).toMatch(/^[0-9a-f]{64}$/);
    expect(store.contains(id)).toBe(true);
  });
});
