import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { MemoryObjectStore, SqlObjectStore } from "../main.ts";

let base: string;
beforeAll(() => {
  base = mkdtempSync(join(tmpdir(), "sql-object-store-test-"));
});

afterAll(() => {
  rmSync(base, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 200,
  });
});

describe("SqlObjectStore (src/object_store/sql.rs)", () => {
  test("in-memory database initializes and basic metadata getters work", () => {
    const store = new SqlObjectStore(":memory:", "sha1");
    expect(store.path).toBe(":memory:");
    expect(store.objectHash).toBe("sha1");
    expect(store.objectFormat).toBe("sha1");
    expect(store.size()).toBe(0);
    expect(store.entries()).toEqual([]);
  });

  test("writeObject computes correct SHA-1 id and object can be retrieved", () => {
    const store = new SqlObjectStore(":memory:", "sha1");
    const data = Buffer.from("git blob content\n");
    const id = store.writeObject("blob", data);

    expect(id).toMatch(/^[0-9a-f]{40}$/);
    expect(store.contains(id)).toBe(true);
    expect(store.size()).toBe(1);
    expect(store.entries()).toEqual([id]);

    const found = store.tryFind(id);
    expect(found).not.toBeNull();
    expect(found!.kind).toBe("blob");
    expect(Buffer.from(found!.data).equals(data)).toBe(true);

    const header = store.tryHeader(id);
    expect(header).not.toBeNull();
    expect(header!.kind).toBe("blob");
    expect(header!.size).toBe(data.length);
  });

  test("contains, tryFind and tryHeader return false / null for missing or invalid objects", () => {
    const store = new SqlObjectStore(":memory:", "sha1");
    const missingId = "0123456789abcdef0123456789abcdef01234567";
    expect(store.contains(missingId)).toBe(false);
    expect(store.tryFind(missingId)).toBeNull();
    expect(store.tryHeader(missingId)).toBeNull();
    expect(store.lastModified(missingId)).toBeNull();
  });

  test("lookupPrefix resolves full or partial prefixes correctly", () => {
    const store = new SqlObjectStore(":memory:", "sha1");
    const id = store.writeObject("blob", Buffer.from("lookup test\n"));

    const full = store.lookupPrefix(id);
    expect(full.found).toBe(true);
    expect(full.ambiguous).toBe(false);
    expect(full.id).toBe(id);

    const prefix = store.lookupPrefix(id.slice(0, 7));
    expect(prefix.found).toBe(true);
    expect(prefix.ambiguous).toBe(false);
    expect(prefix.id).toBe(id);

    const missing = store.lookupPrefix("fffffff");
    expect(missing.found).toBe(false);
    expect(missing.ambiguous).toBe(false);
    expect(missing.id).toBeUndefined();
  });

  test("streaming reader chunks object content", () => {
    const store = new SqlObjectStore(":memory:", "sha1");
    const data = Buffer.from("streaming content chunk test\n");
    const id = store.writeObject("blob", data);

    const reader = store.reader(id);
    expect(reader.kind).toBe("blob");
    expect(reader.size).toBe(data.length);

    const chunks: Buffer[] = [];
    let chunk = reader.read(8);
    while (chunk.length > 0) {
      chunks.push(Buffer.from(chunk));
      chunk = reader.read(8);
    }
    expect(Buffer.concat(chunks).equals(data)).toBe(true);
  });

  test("streaming writer buffers and finalises", () => {
    const store = new SqlObjectStore(":memory:", "sha1");
    const data = Buffer.from("written via writer stream\n");

    const writer = store.writer("blob", data.length);
    writer.write(data.subarray(0, 10));
    writer.write(data.subarray(10));
    const id = writer.finalise();

    expect(id).toMatch(/^[0-9a-f]{40}$/);
    expect(store.contains(id)).toBe(true);
    expect(Buffer.from(store.tryFind(id)!.data).equals(data)).toBe(true);
  });

  test("writer rejects writes exceeding declared size and incomplete writes", () => {
    const store = new SqlObjectStore(":memory:", "sha1");
    const tooLong = store.writer("blob", 5);
    expect(() => tooLong.write(Buffer.from("123456"))).toThrow();

    const tooShort = store.writer("blob", 10);
    tooShort.write(Buffer.from("12345"));
    expect(() => tooShort.finalise()).toThrow();
  });

  test("writeObjectWithId writes under known id", () => {
    const store = new SqlObjectStore(":memory:", "sha1");
    const data = Buffer.from("known id content\n");
    const id = store.writeObject("blob", data);

    const sameId = store.writeObjectWithId("blob", data, id);
    expect(sameId).toBe(id);
    expect(store.size()).toBe(1);
  });

  test("lastModified timestamp is recorded and updated", () => {
    const store = new SqlObjectStore(":memory:", "sha1");
    const id = store.writeObject("blob", Buffer.from("time test\n"));
    const time = store.lastModified(id);
    expect(typeof time).toBe("number");
    expect(time).toBeGreaterThan(0);
  });

  test("copy duplicates object under another id", () => {
    const store = new SqlObjectStore(":memory:", "sha1");
    const idA = store.writeObject("blob", Buffer.from("copy me\n"));
    const targetId = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    const copiedId = store.copy(idA, targetId);
    expect(copiedId).toBe(targetId);
    expect(store.contains(targetId)).toBe(true);
    expect(store.size()).toBe(2);
    expect(Buffer.from(store.tryFind(targetId)!.data).equals(Buffer.from("copy me\n"))).toBe(true);
  });

  test("copy throws if source object does not exist", () => {
    const store = new SqlObjectStore(":memory:", "sha1");
    expect(() =>
      store.copy(
        "1111111111111111111111111111111111111111",
        "2222222222222222222222222222222222222222",
      ),
    ).toThrow();
  });

  test("objectPath throws because SQLite store has no loose file paths", () => {
    const store = new SqlObjectStore(":memory:", "sha1");
    expect(() => store.objectPath("abcd")).toThrow();
  });

  test("packfile dump and load round-trip", () => {
    const storeA = new SqlObjectStore(":memory:", "sha1");
    const id1 = storeA.writeObject("blob", Buffer.from("pack item 1\n"));
    const id2 = storeA.writeObject("blob", Buffer.from("pack item 2\n"));
    const pack = storeA.dump();

    const storeB = new SqlObjectStore(":memory:", "sha1");
    expect(storeB.size()).toBe(0);
    storeB.load(pack);
    expect(storeB.size()).toBe(2);
    expect(storeB.contains(id1)).toBe(true);
    expect(storeB.contains(id2)).toBe(true);
  });

  test("importStream imports pack generated from MemoryObjectStore", () => {
    const memStore = new MemoryObjectStore();
    const id1 = memStore.writeObject("blob", Buffer.from("mem 1\n"));
    const id2 = memStore.writeObject("blob", Buffer.from("mem 2\n"));
    const pack = memStore.dump();

    const sqlStore = new SqlObjectStore(":memory:", "sha1");
    sqlStore.importStream(pack);
    expect(sqlStore.size()).toBe(2);
    expect(sqlStore.contains(id1)).toBe(true);
    expect(sqlStore.contains(id2)).toBe(true);
  });

  test("reset clears all objects in the database", () => {
    const store = new SqlObjectStore(":memory:", "sha1");
    store.writeObject("blob", Buffer.from("reset test 1\n"));
    store.writeObject("blob", Buffer.from("reset test 2\n"));
    expect(store.size()).toBe(2);

    store.reset();
    expect(store.size()).toBe(0);
    expect(store.entries()).toEqual([]);
  });

  test("file-based SQLite database persists objects across store instances", () => {
    const dbPath = join(base, "file_test.sqlite");
    const store1 = new SqlObjectStore(dbPath, "sha1");
    const content = Buffer.from("persistent on disk\n");
    const id = store1.writeObject("blob", content);

    expect(existsSync(dbPath)).toBe(true);
    expect(store1.contains(id)).toBe(true);

    // Open a second instance pointing to the same SQLite file
    const store2 = new SqlObjectStore(dbPath, "sha1");
    expect(store2.size()).toBe(1);
    expect(store2.contains(id)).toBe(true);
    expect(Buffer.from(store2.tryFind(id)!.data).equals(content)).toBe(true);
  });

  test("SHA-256 kind produces 64-character hex object ids", () => {
    const store = new SqlObjectStore(":memory:", "sha256");
    expect(store.objectHash).toBe("sha256");
    expect(store.objectFormat).toBe("sha256");

    const content = Buffer.from("sha256 content\n");
    const id = store.writeObject("blob", content);
    expect(id).toMatch(/^[0-9a-f]{64}$/);
    expect(store.contains(id)).toBe(true);

    const found = store.tryFind(id);
    expect(found).not.toBeNull();
    expect(Buffer.from(found!.data).equals(content)).toBe(true);
  });
});
