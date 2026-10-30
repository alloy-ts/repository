import { execSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { LooseObjectStore, MemoryObjectStore, ObjectType, Repository } from "../main.ts";

const realRoot = process.cwd();
const MISSING_ID = "0000000000000000000000000000000000000000";

let base: string;
let objectsDir: string;
let headId: string;
let sha256RepoDir: string;
let sha256BlobId: string;

beforeAll(() => {
  base = mkdtempSync(join(tmpdir(), "loose-store-"));

  objectsDir = join(
    realRoot,
    execSync("git rev-parse --git-common-dir", {
      cwd: realRoot,
      encoding: "utf8",
    }).trim(),
    "objects",
  );
  const mainRepo = new Repository();
  headId = mainRepo.writeObject(ObjectType.Blob, Buffer.from("loose-store real repo blob\n"));

  // A small SHA-256 repository with a written blob, so the loose store can be opened in that hash.
  sha256RepoDir = join(base, "sha256-repo");
  const repo = Repository.initOpts(sha256RepoDir, { objectFormat: "sha256" });
  sha256BlobId = repo.writeObject(ObjectType.Blob, Buffer.from("loose-store sha256 blob\n"));
});

afterAll(() => {
  rmSync(base, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 200,
  });
});

describe("LooseObjectStore (src/object_store/loose.rs)", () => {
  test("reads a real repository's loose objects", () => {
    const store = new LooseObjectStore(objectsDir);
    expect(store.path).toBe(objectsDir);
    expect(store.objectHash).toBe("sha1");
    expect(store.objectFormat).toBe("sha1");

    expect(store.contains(headId)).toBe(true);
    expect(store.contains(MISSING_ID)).toBe(false);

    const found = store.tryFind(headId);
    expect(found).not.toBeNull();
    expect(found!.kind).toBe("blob");
    expect(Buffer.from(found!.data).length).toBeGreaterThan(0);

    const header = store.tryHeader(headId);
    expect(header).not.toBeNull();
    expect(header!.kind).toBe("blob");
    expect(header!.size).toBe(Buffer.from(found!.data).length);
  });

  test("lookupPrefix() resolves the head id", () => {
    const store = new LooseObjectStore(objectsDir);
    const lookup = store.lookupPrefix(headId);
    expect(lookup.found).toBe(true);
    expect(lookup.ambiguous).toBe(false);
    expect(lookup.id).toBe(headId);
  });

  test("objectPath() returns the loose object file path", () => {
    const store = new LooseObjectStore(objectsDir);
    const p = store.objectPath(headId);
    expect(p).not.toMatch(/\.git(\/|\\)objects(\/|\\)pack/); // not under pack
    expect(typeof p).toBe("string");
  });

  test("reader() streams a commit object", () => {
    const store = new LooseObjectStore(objectsDir);
    const reader = store.reader(headId);
    expect(reader.kind).toBe("blob");
    expect(reader.size).toBeGreaterThan(0);
    let bytes = 0;
    let chunk = reader.read(64);
    while (chunk.length > 0) {
      bytes += chunk.length;
      chunk = reader.read(64);
    }
    expect(bytes).toBe(reader.size);
  });

  test("writer() round-trips a blob", () => {
    const dir = join(base, "loose-rw");
    const store = new LooseObjectStore(dir);
    const blob = Buffer.from("loose-store blob\n");
    const id = store.writer("blob", 0).finalize(); // empty write -> zero-length blob
    // Write real content via the two-phase writer.
    const w = store.writer("blob", blob.length);
    w.write(blob);
    const realId = w.finalize();
    expect(realId).toMatch(/^[0-9a-f]{40}$/);
    expect(store.contains(realId)).toBe(true);
    const found = store.tryFind(realId);
    expect(found!.kind).toBe("blob");
    expect(Buffer.from(found!.data).equals(blob)).toBe(true);
    void id;
  });

  test("writeObject() computes the correct id", () => {
    const dir = join(base, "loose-write");
    const store = new LooseObjectStore(dir);
    const blob = Buffer.from("compute-id-blob\n");
    const id = store.writeObject("blob", blob);
    expect(id).toMatch(/^[0-9a-f]{40}$/);
    expect(store.tryFind(id)!.data.equals(blob)).toBe(true);
  });

  test("opens a SHA-256 loose store and reports its format", () => {
    const store = new LooseObjectStore(join(sha256RepoDir, ".git", "objects"), {
      objectHash: "sha256",
    });
    expect(store.objectHash).toBe("sha256");
    expect(store.objectFormat).toBe("sha256");
    expect(store.contains(sha256BlobId)).toBe(true);
    const header = store.tryHeader(sha256BlobId);
    expect(header!.kind).toBe("blob");
    expect(header!.size).toBe(24);
  });

  test("importStream() ingests a pack produced by MemoryObjectStore.dump()", () => {
    const mem = new MemoryObjectStore();
    const blob = Buffer.from("import-stream-blob\n");
    const id = mem.writeObject("blob", blob);
    mem.writeObject("tree", Buffer.from(""));
    const pack = mem.dump();

    const dir = join(base, "loose-import");
    const store = new LooseObjectStore(dir);
    store.importStream(pack);
    expect(store.contains(id)).toBe(true);
    expect(Buffer.from(store.tryFind(id)!.data).equals(blob)).toBe(true);
  });

  test("rejects an invalid object hash", () => {
    expect(() => new LooseObjectStore(objectsDir, "blake256")).toThrow();
  });
});
