import { execSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { ObjectDatabase, ObjectHandle, ObjectStore } from "./main.ts";

const realRoot = process.cwd();
const MISSING_ID = "0000000000000000000000000000000000000000";

let base: string;
let objectStore: ObjectStore;
let objectsDir: string;
let headId: string;

beforeAll(() => {
  base = mkdtempSync(join(tmpdir(), "repo-store-"));
  const looseObjectsDir = join(base, "loose-objects");
  mkdirSync(looseObjectsDir, { recursive: true });
  objectStore = new ObjectStore(looseObjectsDir);

  objectsDir = join(
    realRoot,
    execSync("git rev-parse --git-common-dir", {
      cwd: realRoot,
      encoding: "utf8",
    }).trim(),
    "objects",
  );
  headId = execSync("git rev-parse HEAD", { cwd: realRoot, encoding: "utf8" }).trim();
});

afterAll(() => {
  rmSync(base, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 200,
  });
});

describe("ObjectStore (src/store.rs)", () => {
  const data = Buffer.from("store-test-blob\n");

  test("writeObject() round-trips a loose object", () => {
    const id = objectStore.writeObject("blob", data);
    expect(id).toMatch(/^[0-9a-f]{40}$/);

    expect(objectStore.contains(id)).toBe(true);
    expect(objectStore.contains(MISSING_ID)).toBe(false);

    const found = objectStore.tryFind(id);
    expect(found).not.toBeNull();
    expect(found!.kind).toBe("blob");
    expect(Buffer.from(found!.data).equals(data)).toBe(true);

    const header = objectStore.tryHeader(id);
    expect(header).not.toBeNull();
    expect(header!.kind).toBe("blob");
    expect(header!.size).toBe(data.length);

    // A missing (but well-formed) id yields null, not an error.
    expect(objectStore.tryFind(MISSING_ID)).toBeNull();
    expect(objectStore.tryHeader(MISSING_ID)).toBeNull();
  });

  test("objectPath() points at the sharded loose file", () => {
    const id = objectStore.writeObject("blob", data);
    const path = objectStore.objectPath(id);
    // Loose objects are stored sharded as <2-hex>/<38-hex>, so the contiguous id string does not
    // appear; assert the sharded form instead.
    expect(path).toContain(join(id.slice(0, 2), id.slice(2)));
    expect(path).toContain(join(base, "loose-objects"));
  });

  test("lookupPrefix() resolves a unique prefix", () => {
    const id = objectStore.writeObject("blob", data);
    const lookup = objectStore.lookupPrefix(id.slice(0, 12));
    expect(lookup.found).toBe(true);
    expect(lookup.ambiguous).toBe(false);
    expect(lookup.id).toBe(id);

    const none = objectStore.lookupPrefix("ffffffffffff");
    expect(none.found).toBe(false);
  });

  test("getters report the configured state", () => {
    expect(objectStore.path).toContain("loose-objects");
    expect(objectStore.objectHash).toBe("sha1");
  });

  test("unsupported object hash is rejected", () => {
    const dir = join(base, "sha256-loose");
    mkdirSync(dir, { recursive: true });
    expect(() => new ObjectStore(dir, "sha256")).toThrow();
  });
});

describe("ObjectDatabase / ObjectHandle (src/store.rs)", () => {
  let db: ObjectDatabase;

  beforeAll(() => {
    db = new ObjectDatabase(objectsDir);
  });

  test("constructor surfaces a real repository object (loose or packed)", () => {
    expect(db.path).toBe(objectsDir);
    expect(db.objectHash).toBe("sha1");

    expect(db.contains(headId)).toBe(true);
    expect(db.contains(MISSING_ID)).toBe(false);

    const found = db.tryFind(headId);
    expect(found).not.toBeNull();
    expect(found!.kind).toBe("commit");
    expect(Buffer.from(found!.data).length).toBeGreaterThan(0);

    const header = db.tryHeader(headId);
    expect(header).not.toBeNull();
    expect(header!.kind).toBe("commit");
    expect(header!.size).toBeGreaterThan(0);
    // tryFind body length must match the header size.
    expect(Buffer.from(found!.data).length).toBe(header!.size);
  });

  test("lookupPrefix() resolves the head id", () => {
    const lookup = db.lookupPrefix(headId);
    expect(lookup.found).toBe(true);
    expect(lookup.ambiguous).toBe(false);
    expect(lookup.id).toBe(headId);

    // A very short prefix is at least "not uniquely found" in a populated repo.
    const short = db.lookupPrefix(headId.slice(0, 1));
    expect(short.found).toBe(false);
  });

  test("pack/object caches can be toggled", () => {
    expect(db.hasPackCache()).toBe(false);
    expect(db.hasObjectCache()).toBe(false);

    db.setPackCache("none");
    expect(db.hasPackCache()).toBe(true);
    db.unsetPackCache();
    expect(db.hasPackCache()).toBe(false);

    db.setObjectCache("none");
    expect(db.hasObjectCache()).toBe(true);
    db.unsetObjectCache();
    expect(db.hasObjectCache()).toBe(false);

    expect(() => db.setPackCache("lru")).toThrow();
  });

  test("intoArc() returns an equivalent cached database", () => {
    const other = db.intoArc();
    expect(other).toBeInstanceOf(ObjectDatabase);
    expect(other.path).toBe(objectsDir);
    expect(other.contains(headId)).toBe(true);
  });

  test("intoInner() exposes the bare ObjectHandle", () => {
    const handle: ObjectHandle = db.intoInner();
    expect(handle).toBeInstanceOf(ObjectHandle);
    expect(handle.path).toBe(objectsDir);
    expect(handle.objectHash).toBe("sha1");

    expect(handle.contains(headId)).toBe(true);
    const found = handle.tryFind(headId);
    expect(found).not.toBeNull();
    expect(found!.kind).toBe("commit");
    const header = handle.tryHeader(headId);
    expect(header).not.toBeNull();
    expect(header!.kind).toBe("commit");

    const lookup = handle.lookupPrefix(headId);
    expect(lookup.found).toBe(true);
    expect(lookup.id).toBe(headId);

    expect(handle.tryFind(MISSING_ID)).toBeNull();
    expect(handle.tryHeader(MISSING_ID)).toBeNull();
  });

  test("structure() and alternate_db_paths() return arrays", () => {
    const handle: ObjectHandle = db.intoInner();
    const records = handle.structure();
    expect(Array.isArray(records)).toBe(true);
    expect(records.length).toBeGreaterThan(0);
    for (const r of records) {
      expect(["loose", "index", "multi_index", "empty"]).toContain(r.kind);
    }

    const alternates = handle.alternate_db_paths();
    expect(Array.isArray(alternates)).toBe(true);
  });

  test("unsupported object hash is rejected", () => {
    expect(() => new ObjectDatabase(objectsDir, { objectHash: "sha256" })).toThrow();
  });
});
