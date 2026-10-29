import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { LooseObjectStore, MemoryObjectStore } from "../main.ts";

/**
 * `LooseObjectStore` is now a persistent, prolly-backed object database (the old
 * git-loose store's successor). It no longer reads a real `.git/objects` directory
 * and has no per-object loose file, so every test uses a private node-store dir.
 */
const dirs: string[] = [];

function newDir(label = "loose-store"): string {
  const dir = mkdtempSync(join(tmpdir(), `${label}-`));
  dirs.push(dir);
  return dir;
}

afterAll(() => {
  for (const dir of dirs) {
    rmSync(dir, {
      recursive: true,
      force: true,
      maxRetries: 10,
      retryDelay: 200,
    });
  }
});

describe("LooseObjectStore (src/object_store/loose.rs)", () => {
  test("path and objectFormat report the node store directory", () => {
    const dir = newDir();
    const store = new LooseObjectStore(dir);
    expect(store.path).toBe(dir);
    expect(store.objectHash).toBe("sha1");
    expect(store.objectFormat).toBe("sha1");
  });

  test("writeObject() computes the correct id and the object is readable", () => {
    const store = new LooseObjectStore(newDir());
    const blob = Buffer.from("loose-store blob\n");
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
    const store = new LooseObjectStore(newDir());
    const blob = Buffer.from("explicit-id-blob\n");
    const id = store.writeObject("blob", blob);
    const again = store.writeObjectWithId("blob", blob, id);
    expect(again).toBe(id);
    expect(store.size()).toBe(1);
  });

  test("tryHeader() reports kind and size, and null for a missing object", () => {
    const store = new LooseObjectStore(newDir());
    const blob = Buffer.from("header-blob\n");
    const id = store.writeObject("blob", blob);

    const header = store.tryHeader(id);
    expect(header).not.toBeNull();
    expect(header!.kind).toBe("blob");
    expect(header!.size).toBe(blob.length);
    expect(store.tryHeader("f".repeat(id.length))).toBeNull();
  });

  test("reader() streams an object", () => {
    const store = new LooseObjectStore(newDir());
    const blob = Buffer.from("streamed-loose-blob\n");
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
    const store = new LooseObjectStore(newDir());
    const blob = Buffer.from("loose-writer-blob\n");
    const w = store.writer("blob", blob.length);
    w.write(blob.subarray(0, 6));
    w.write(blob.subarray(6));
    const id = w.finalize();
    expect(store.contains(id)).toBe(true);
    expect(Buffer.from(store.tryFind(id)!.data).equals(blob)).toBe(true);

    const tooBig = store.writer("blob", 3);
    expect(() => tooBig.write(Buffer.from("toolong"))).toThrow();
    const tooShort = store.writer("blob", 5);
    tooShort.write(Buffer.from("abc"));
    expect(() => tooShort.finalize()).toThrow();
  });

  test("objectPath() errors for the prolly-backed store", () => {
    const store = new LooseObjectStore(newDir());
    const id = store.writeObject("blob", Buffer.from("x\n"));
    expect(() => store.objectPath(id)).toThrow();
    expect(() => store.objectPath("abc")).toThrow();
  });

  test("lookupPrefix() resolves a unique prefix and reports ambiguity", () => {
    const store = new LooseObjectStore(newDir());
    const ids: string[] = [];
    for (let i = 0; i < 20; i++) {
      ids.push(store.writeObject("blob", Buffer.from(`prefix-blob-${i}\n`)));
    }
    const prefix = ids[0].slice(0, 8);
    const hit = store.lookupPrefix(prefix);
    expect(hit.found).toBe(true);
    expect(hit.ambiguous).toBe(false);
    expect(hit.id).toBe(ids[0]);

    // A short prefix shared by multiple ids is ambiguous.
    const ambiguous = store.lookupPrefix(ids[0].slice(0, 1));
    expect(ambiguous.found).toBe(false);
    expect(ambiguous.ambiguous).toBe(true);
  });

  test("importStream() ingests a pack produced by MemoryObjectStore.dump()", () => {
    const mem = new MemoryObjectStore();
    const blob = Buffer.from("import-stream-blob\n");
    const id = mem.writeObject("blob", blob);
    mem.writeObject("tree", Buffer.from(""));
    const pack = mem.dump();

    const store = new LooseObjectStore(newDir());
    store.importStream(pack);
    expect(store.contains(id)).toBe(true);
    expect(Buffer.from(store.tryFind(id)!.data).equals(blob)).toBe(true);
  });

  test("dump()/load() round-trip the whole store", () => {
    const store = new LooseObjectStore(newDir());
    const a = store.writeObject("blob", Buffer.from("alpha\n"));
    const b = store.writeObject("blob", Buffer.from("beta\n"));
    const pack = store.dump();
    expect(Buffer.from(pack).length).toBeGreaterThan(0);

    const other = new LooseObjectStore(newDir());
    expect(other.size()).toBe(0);
    other.load(pack);
    expect(other.contains(a)).toBe(true);
    expect(other.contains(b)).toBe(true);
    expect(other.size()).toBe(2);
  });

  test("fork() produces an independent copy of the store", () => {
    const store = new LooseObjectStore(newDir());
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

  test("entries() returns ids in insertion order", () => {
    const store = new LooseObjectStore(newDir());
    const a = store.writeObject("blob", Buffer.from("alpha\n"));
    const b = store.writeObject("blob", Buffer.from("beta\n"));
    const c = store.writeObject("blob", Buffer.from("gamma\n"));
    expect(store.entries()).toEqual([a, b, c]);
  });

  test("lastModified() reports a timestamp for present objects and null otherwise", () => {
    const store = new LooseObjectStore(newDir());
    const id = store.writeObject("blob", Buffer.from("stamped\n"));
    const ts = store.lastModified(id);
    expect(ts).not.toBeNull();
    expect(typeof ts).toBe("number");
    expect(ts!).toBeGreaterThan(0);
    expect(store.lastModified("f".repeat(id.length))).toBeNull();
  });

  test("copy() duplicates an object under a new explicit id", () => {
    const store = new LooseObjectStore(newDir());
    const blob = Buffer.from("copy-me\n");
    const src = store.writeObject("blob", blob);
    const target = "0".repeat(src.length);
    expect(store.copy(src, target)).toBe(target);
    expect(store.contains(target)).toBe(true);
    expect(Buffer.from(store.tryFind(target)!.data).equals(blob)).toBe(true);
    expect(store.size()).toBe(2);
  });

  test("reset() clears the store", () => {
    const store = new LooseObjectStore(newDir());
    store.writeObject("blob", Buffer.from("temporary\n"));
    expect(store.size()).toBe(1);
    store.reset();
    expect(store.size()).toBe(0);
    expect(store.entries()).toEqual([]);
  });

  test("saveRoot() persists the store and a new store at the same path reloads it", () => {
    const dir = newDir();
    const store = new LooseObjectStore(dir);
    const id = store.writeObject("blob", Buffer.from("persisted\n"));
    store.saveRoot();

    const reopened = new LooseObjectStore(dir);
    expect(reopened.size()).toBe(1);
    expect(reopened.contains(id)).toBe(true);
    expect(Buffer.from(reopened.tryFind(id)!.data).equals(Buffer.from("persisted\n"))).toBe(true);
  });

  test("restoreRoot() reloads the persisted root and replaces current contents", () => {
    const dir = newDir();
    const store = new LooseObjectStore(dir);
    const saved = store.writeObject("blob", Buffer.from("saved\n"));
    store.saveRoot();

    store.writeObject("blob", Buffer.from("unsaved\n"));
    expect(store.size()).toBe(2);

    store.restoreRoot();
    expect(store.size()).toBe(1);
    expect(store.contains(saved)).toBe(true);
  });

  test("restoreRoot() throws when no root has been saved", () => {
    const store = new LooseObjectStore(newDir());
    expect(() => store.restoreRoot()).toThrow();
  });

  test("a store without saveRoot() does not see earlier contents on reopen", () => {
    const dir = newDir();
    const store = new LooseObjectStore(dir);
    store.writeObject("blob", Buffer.from("not-persisted\n"));
    const reopened = new LooseObjectStore(dir);
    expect(reopened.size()).toBe(0);
  });

  test("SHA-256 variant reports its format and uses 64-hex ids", () => {
    const store = new LooseObjectStore(newDir(), "sha256");
    expect(store.objectHash).toBe("sha256");
    expect(store.objectFormat).toBe("sha256");
    const blob = Buffer.from("sha256-loose-blob\n");
    const id = store.writeObject("blob", blob);
    expect(id).toMatch(/^[0-9a-f]{64}$/);
    expect(store.contains(id)).toBe(true);
    const header = store.tryHeader(id);
    expect(header!.kind).toBe("blob");
    expect(header!.size).toBe(blob.length);
  });

  test("rejects an invalid object hash", () => {
    expect(() => new LooseObjectStore(newDir(), "blake256")).toThrow();
  });
});
