import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { deflateSync } from "node:zlib";
import { afterAll, describe, expect, test } from "vite-plus/test";

import { Pack, PackEntry, Repository } from "./main.ts";

// Build a minimal, valid pack containing a single base object of the given git type.
// A pack is: "PACK" + version (4 bytes, 2) + object count (4 bytes, 1) + object entries + 20-byte SHA1 trailer.
// Each object entry is a variable-length type+size header followed by the zlib-compressed data.
// Object type ids: 1=commit, 2=tree, 3=blob, 4=tag.
function buildPack(content: Buffer, typeId: number): Buffer {
  const compressed = deflateSync(content);
  const size = content.length;
  const header: number[] = [];
  let c = (size & 0x0f) | (typeId << 4);
  let s = size >> 4;
  while (s > 0) {
    header.push(c | 0x80);
    c = s & 0x7f;
    s >>= 7;
  }
  header.push(c);
  const entryHeader = Buffer.from(header);

  const packHeader = Buffer.concat([
    Buffer.from("PACK"),
    Buffer.from([0, 0, 0, 2]), // version 2
    Buffer.from([0, 0, 0, 1]), // 1 object
  ]);
  const data = Buffer.concat([packHeader, entryHeader, compressed]);
  const trailer = createHash("sha1").update(data).digest();
  return Buffer.concat([data, trailer]);
}

// A pack data file begins with a 12-byte header: magic "PACK", u32 version, u32 object count.
// The first entry therefore starts at absolute offset 12.
const HEADER_LEN = 12;

describe("PackEntry / PackEntryHeader (src/pack.rs)", () => {
  describe("fromBytes + base object decoding", () => {
    test("decodes a blob entry and exposes its offsets/sizes", () => {
      const pack = buildPack(Buffer.from("hello"), 3);
      // `data` must start at the entry offset; `packOffset` is the absolute offset within the pack.
      const entry = PackEntry.fromBytes(pack.subarray(HEADER_LEN), HEADER_LEN, "sha1");

      expect(entry.decompressedSize).toBe(5);
      expect(entry.encodedHeaderSize).toBe(1);
      expect(entry.headerSize()).toBe(1);
      expect(entry.dataOffset).toBe(HEADER_LEN + 1);
      expect(entry.packOffset()).toBe(HEADER_LEN);
    });

    test("surfaces a base blob's header correctly", () => {
      const pack = buildPack(Buffer.from("hello"), 3);
      const entry = PackEntry.fromBytes(pack.subarray(HEADER_LEN), HEADER_LEN, "sha1");
      const header = entry.header;

      expect(header.kind).toBe("blob");
      expect(header.isDelta).toBe(false);
      expect(header.isBase).toBe(true);
      expect(header.objectKind).toBe("blob");
      expect(header.typeId).toBe(3);
      expect(header.baseId).toBeNull();
      expect(header.baseDistance).toBeNull();
    });

    test("decodes a commit entry with the right type id", () => {
      const pack = buildPack(Buffer.from("tree abc\n"), 1);
      const entry = PackEntry.fromBytes(pack.subarray(HEADER_LEN), HEADER_LEN, "sha1");
      const header = entry.header;

      expect(header.kind).toBe("commit");
      expect(header.typeId).toBe(1);
      expect(header.objectKind).toBe("commit");
      expect(entry.decompressedSize).toBe("tree abc\n".length);
    });

    test("checkedBasePackOffset respects the pack header boundary", () => {
      const pack = buildPack(Buffer.from("hi"), 3);
      const entry = PackEntry.fromBytes(pack.subarray(HEADER_LEN), HEADER_LEN, "sha1");
      // The first entry sits at pack offset 12, so its base would land at or before the 12-byte
      // PACK header, which is rejected -> always null here.
      expect(entry.checkedBasePackOffset(0)).toBeNull();
      expect(entry.checkedBasePackOffset(1)).toBeNull();
    });
  });

  describe("error handling", () => {
    test("rejects sha256 (not enabled in this build)", () => {
      const pack = buildPack(Buffer.from("hi"), 3);
      expect(() => PackEntry.fromBytes(pack.subarray(HEADER_LEN), HEADER_LEN, "sha256")).toThrow();
    });

    test("rejects an unknown hash kind", () => {
      const pack = buildPack(Buffer.from("hi"), 3);
      expect(() => PackEntry.fromBytes(pack.subarray(HEADER_LEN), HEADER_LEN, "md5")).toThrow();
    });

    test("rejects a negative pack offset", () => {
      const pack = buildPack(Buffer.from("hi"), 3);
      expect(() => PackEntry.fromBytes(pack.subarray(HEADER_LEN), -1, "sha1")).toThrow();
    });

    // Reading from offset 9999 parses the "PACK" magic as a (invalid) entry type, which is rejected.
    test("throws when the offset is past the end of the data", () => {
      const pack = buildPack(Buffer.from("hi"), 3);
      expect(() => PackEntry.fromBytes(pack, 9999, "sha1")).toThrow();
    });
  });
});

// The builder only reads objects, so it can run against this repository directly.
const repoRoot = process.cwd();
const tmpDirs: string[] = [];

function makeTmpDir(): string {
  const dir = mkdtempSync(join(tmpdir(), "pack-build-"));
  tmpDirs.push(dir);
  return dir;
}

afterAll(() => {
  for (const dir of tmpDirs) {
    rmSync(dir, { recursive: true, force: true });
  }
});

describe("Pack builder (src/pack.rs)", () => {
  test("addCommit + toBuffer produces a valid, non-empty pack", async () => {
    const repo = Repository.open(repoRoot);
    const pack = Pack.build(repo);
    pack.addCommit(repo.headId());

    const result = await pack.toBuffer();

    expect(result.data.subarray(0, 4).toString()).toBe("PACK");
    expect(result.objectCount).toBeGreaterThan(1); // commit + its tree contents
    expect(result.packHash).toMatch(/^[0-9a-f]{40}$/);
    expect(result.bytesWritten).toBe(result.data.length);
  });

  test("addObject / addObjects insert objects as-is", async () => {
    const repo = Repository.open(repoRoot);
    const pack = Pack.build(repo);
    pack.addObject(repo.headId());
    const single = await pack.toBuffer();
    expect(single.objectCount).toBe(1);

    const other = Pack.build(repo);
    other.addObjects([repo.headId()]);
    expect((await other.toBuffer()).objectCount).toBe(1);
  });

  test("an empty plan yields the empty packfile", async () => {
    const repo = Repository.open(repoRoot);
    const result = await Pack.build(repo).toBuffer();

    expect(result.objectCount).toBe(0);
    expect(result.data.subarray(0, 4).toString()).toBe("PACK");
    // 12-byte header + 20-byte sha1 trailer, no objects.
    expect(result.data.length).toBe(32);
  });

  test("clear() empties the accumulated plan", async () => {
    const repo = Repository.open(repoRoot);
    const pack = Pack.build(repo);
    pack.addCommit(repo.headId());
    pack.clear();

    expect((await pack.toBuffer()).objectCount).toBe(0);
  });

  test("writeToDirectory writes a .pack and its .idx", async () => {
    const repo = Repository.open(repoRoot);
    const dir = makeTmpDir();
    const pack = Pack.build(repo);
    pack.addCommit(repo.headId());

    const result = await pack.writeToDirectory(dir);

    expect(result.objectCount).toBeGreaterThan(1);
    expect(result.packPath?.endsWith(".pack")).toBe(true);
    expect(result.indexPath?.endsWith(".idx")).toBe(true);
    expect(result.packHash).toMatch(/^[0-9a-f]{40}$/);
    expect(existsSync(result.packPath!)).toBe(true);
    expect(existsSync(result.indexPath!)).toBe(true);
  });

  test("accepts a progress callback and build options", async () => {
    const repo = Repository.open(repoRoot);
    const seen: unknown[] = [];
    const pack = Pack.build(repo, { threads: 1, compressionLevel: 1 });
    pack.addCommit(repo.headId());

    // Progress delivery is best-effort; the operation must still complete.
    const result = await pack.toBuffer((event) => {
      seen.push(event);
    });

    expect(result.objectCount).toBeGreaterThan(1);
  });
});
