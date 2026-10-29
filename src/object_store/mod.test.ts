import { execSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { ObjectDatabase, ObjectHandle } from "../main.ts";

const realRoot = process.cwd();
const MISSING_ID = "0000000000000000000000000000000000000000";

let base: string;
let objectsDir: string;
let headId: string;

beforeAll(() => {
  base = mkdtempSync(join(tmpdir(), "repo-store-"));

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

describe("ObjectDatabase / ObjectHandle (src/object_store/mod.rs)", () => {
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

  test("metrics() returns store resource-usage counters", () => {
    const handle: ObjectHandle = db.intoInner();
    const m = handle.metrics();
    expect(typeof m.numHandles).toBe("number");
    expect(typeof m.numRefreshes).toBe("number");
    expect(typeof m.openReachableIndices).toBe("number");
    expect(typeof m.knownReachableIndices).toBe("number");
    expect(typeof m.openReachablePacks).toBe("number");
    expect(typeof m.knownPacks).toBe("number");
    expect(typeof m.unusedSlots).toBe("number");
    expect(typeof m.unreachableIndices).toBe("number");
    expect(typeof m.unreachablePacks).toBe("number");
    expect(typeof m.looseDbs).toBe("number");

    // ObjectDatabase delegates to the same backing store.
    const m2 = db.metrics();
    expect(typeof m2.numHandles).toBe("number");
  });

  test("entries() lists object ids, including the head commit", () => {
    const handle: ObjectHandle = db.intoInner();
    const ids = handle.entries();
    expect(Array.isArray(ids)).toBe(true);
    expect(ids.length).toBeGreaterThan(0);
    expect(ids).toContain(headId);
    for (const id of ids) {
      expect(id).toMatch(/^[0-9a-f]{40}$/);
    }

    const ids2 = db.entries();
    expect(ids2).toContain(headId);
  });

  test("statistics() tallies objects by kind and storage", () => {
    const handle: ObjectHandle = db.intoInner();
    const s = handle.statistics();
    expect(s.totalObjects).toBeGreaterThan(0);
    expect(s.looseObjects + s.packedObjects).toBe(s.totalObjects);
    expect(s.commits + s.trees + s.tags + s.blobs).toBe(s.totalObjects);
    // Sizes and delta tallies are non-negative numbers.
    expect(s.commitsSize).toBeGreaterThanOrEqual(0);
    expect(s.treesSize).toBeGreaterThanOrEqual(0);
    expect(s.tagsSize).toBeGreaterThanOrEqual(0);
    expect(s.blobsSize).toBeGreaterThanOrEqual(0);
    expect(s.totalDeltaChainLength).toBeGreaterThanOrEqual(0);
    expect(s.packedDeltaObjects).toBeGreaterThanOrEqual(0);

    // ObjectDatabase delegates to the same backing store.
    const s2 = db.statistics();
    expect(s2.totalObjects).toBe(s.totalObjects);
  });

  test("info() combines path, hash, structure, and metrics", () => {
    const handle: ObjectHandle = db.intoInner();
    const info = handle.info();
    expect(info.path).toBe(objectsDir);
    expect(info.objectHash).toBe("sha1");
    expect(typeof info.useMultiPackIndex).toBe("boolean");
    expect(Array.isArray(info.structure)).toBe(true);
    expect(info.structure.length).toBeGreaterThan(0);
    expect(typeof info.metrics.numHandles).toBe("number");

    const info2 = db.info();
    expect(info2.path).toBe(objectsDir);
    expect(info2.objectHash).toBe("sha1");
  });

  test("sha256 object hash is accepted", () => {
    const dir = join(base, "sha256-db");
    const db256 = new ObjectDatabase(dir, { objectHash: "sha256" });
    expect(db256.objectHash).toBe("sha256");
  });

  test("reader() streams a commit object from the real database", () => {
    const reader = db.reader(headId);
    expect(reader.kind).toBe("commit");
    expect(reader.size).toBeGreaterThan(0);

    let bytes = 0;
    let chunk = reader.read(256);
    while (chunk.length > 0) {
      bytes += chunk.length;
      chunk = reader.read(256);
    }
    expect(bytes).toBe(reader.size);
  });

  test("ObjectHandle reader()/writer() round-trip a blob in a temp db", () => {
    const dir = join(base, "handle-rw");
    const tmpDb = new ObjectDatabase(dir);
    const handle: ObjectHandle = tmpDb.intoInner();
    const blob = Buffer.from("handle-writer-blob\n");

    const writer = handle.writer("blob", blob.length);
    writer.write(blob.subarray(0, 5));
    writer.write(blob.subarray(5));
    const id = writer.finalize();
    expect(id).toMatch(/^[0-9a-f]{40}$/);
    expect(handle.contains(id)).toBe(true);

    const reader = handle.reader(id);
    expect(reader.kind).toBe("blob");
    expect(reader.size).toBe(blob.length);
    const chunks: Buffer[] = [];
    let chunk = reader.read(3);
    while (chunk.length > 0) {
      chunks.push(Buffer.from(chunk));
      chunk = reader.read(3);
    }
    expect(Buffer.concat(chunks).equals(blob)).toBe(true);
  });

  test("ObjectDatabase reader()/writer() round-trip a blob", () => {
    const dir = join(base, "db-rw");
    const tmpDb = new ObjectDatabase(dir);
    const blob = Buffer.from("db-writer-blob\n");

    const writer = tmpDb.writer("blob", blob.length);
    writer.write(blob);
    const id = writer.finalize();
    expect(tmpDb.contains(id)).toBe(true);

    const reader = tmpDb.reader(id);
    expect(reader.kind).toBe("blob");
    const chunks: Buffer[] = [];
    let chunk = reader.read(7);
    while (chunk.length > 0) {
      chunks.push(Buffer.from(chunk));
      chunk = reader.read(7);
    }
    expect(Buffer.concat(chunks).equals(blob)).toBe(true);
  });
});
