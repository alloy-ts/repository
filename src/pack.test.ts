import { createHash } from "node:crypto";
import { deflateSync } from "node:zlib";
import { describe, expect, test } from "vite-plus/test";

import { PackEntry } from "./main.ts";

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
