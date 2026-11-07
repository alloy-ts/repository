/**
 * Tests for the buffered, tokio-style async I/O bindings (`ObjectStoreBufReader` /
 * `ObjectStoreBufWriter` in `src/object_store/buffered.rs`).
 *
 * These wrap `object_store::buffered::{BufReader, BufWriter}` and operate directly on the
 * underlying `object_store` backend (bypassing the git fanout), using an arbitrary path. We
 * exercise them against the in-memory (`"memory"`) and local-filesystem backends that
 * `GenericObjectStore::build` provides — no network required.
 */
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, describe, expect, test } from "vite-plus/test";
import { GenericObjectStore, ObjectStoreBufReader, ObjectStoreBufWriter } from "../main.ts";

describe("ObjectStoreBufReader/Writer (src/object_store/buffered.rs)", () => {
  test("write-then-read round-trip (InMemory)", () => {
    const store = new GenericObjectStore("memory", "sha1", false);
    const data = Buffer.from("hello buffered world");

    const writer = new ObjectStoreBufWriter(store, "rt.bin", {
      capacity: 1024,
    });
    expect(writer.write(data)).toBe(data.length);
    writer.shutdown();

    const reader = new ObjectStoreBufReader(store, "rt.bin");
    expect(reader.size).toBe(data.length);

    const out = reader.read(4096);
    expect(out).toEqual(data);
    expect(out.toString("utf8")).toBe("hello buffered world");
  });

  test("reads incrementally and returns empty Buffer at EOF (InMemory)", () => {
    const store = new GenericObjectStore("memory", "sha1", false);
    const data = Buffer.from("abcdefghijklmnop");

    const writer = new ObjectStoreBufWriter(store, "chunk.bin");
    writer.write(data);
    writer.shutdown();

    const reader = new ObjectStoreBufReader(store, "chunk.bin");
    const collected: Buffer[] = [];
    let part: Buffer;
    // Read 4 bytes at a time; an empty Buffer signals EOF and must terminate the loop.
    while ((part = reader.read(4)).length > 0) {
      collected.push(part);
    }
    expect(Buffer.concat(collected).toString("utf8")).toBe("abcdefghijklmnop");
  });

  test("seek Start / Current / End (InMemory)", () => {
    const store = new GenericObjectStore("memory", "sha1", false);
    const data = Buffer.from(Array.from({ length: 100 }, (_, i) => i));

    const writer = new ObjectStoreBufWriter(store, "seek.bin");
    writer.write(data);
    writer.shutdown();

    const reader = new ObjectStoreBufReader(store, "seek.bin");
    expect(reader.size).toBe(100);

    // Start-relative.
    expect(reader.seek(10, "Start")).toBe(10);
    expect(reader.read(5)).toEqual(data.subarray(10, 15));

    // Current-relative (from 15, back 3 → 12).
    expect(reader.seek(-3, "Current")).toBe(12);
    expect(reader.read(2)).toEqual(data.subarray(12, 14));

    // End-relative (last 10 bytes).
    expect(reader.seek(-10, "End")).toBe(90);
    expect(reader.read(10)).toEqual(data.subarray(90, 100));
  });

  test("seek beyond EOF reads as empty (InMemory)", () => {
    const store = new GenericObjectStore("memory", "sha1", false);
    const data = Buffer.from("short");

    const writer = new ObjectStoreBufWriter(store, "eof.bin");
    writer.write(data);
    writer.shutdown();

    const reader = new ObjectStoreBufReader(store, "eof.bin");
    reader.seek(1_000, "Start");
    expect(reader.read(16).length).toBe(0);
    expect(reader.size).toBe(data.length);
  });

  test("overflows capacity into a multipart upload, preserving order (InMemory)", () => {
    const store = new GenericObjectStore("memory", "sha1", false);
    const chunkA = Buffer.alloc(80, 1);
    const chunkB = Buffer.alloc(80, 2);
    const chunkC = Buffer.alloc(80, 3);

    // capacity 100 < chunkA+chunkB, so the second write spills into a multipart upload.
    const writer = new ObjectStoreBufWriter(store, "multi.bin", {
      capacity: 100,
    });
    writer.write(chunkA);
    writer.write(chunkB);
    writer.write(chunkC);
    writer.shutdown();

    const reader = new ObjectStoreBufReader(store, "multi.bin");
    expect(reader.size).toBe(240);
    const out = reader.read(4096);
    expect(out.length).toBe(240);
    expect(out.subarray(0, 80).every((b) => b === 1)).toBe(true);
    expect(out.subarray(80, 160).every((b) => b === 2)).toBe(true);
    expect(out.subarray(160, 240).every((b) => b === 3)).toBe(true);
  });

  test("abort discards the upload — the object is never visible (InMemory)", () => {
    const store = new GenericObjectStore("memory", "sha1", false);
    const writer = new ObjectStoreBufWriter(store, "abort.bin", {
      capacity: 100,
    });
    writer.write(Buffer.alloc(150, 7)); // exceeds capacity → enters multipart state
    writer.abort();
    // No object was finalised, so constructing a reader fails (NotFound).
    expect(() => new ObjectStoreBufReader(store, "abort.bin")).toThrow();
  });

  test("round-trip and seek on a local filesystem backend", () => {
    const dir = mkdtempSync(join(tmpdir(), "objstore-buffered-"));
    try {
      const store = new GenericObjectStore(dir, "sha1", false);
      const data = Buffer.from("local fs content");

      const writer = new ObjectStoreBufWriter(store, "lf.txt", {
        capacity: 1024,
      });
      writer.write(data);
      writer.shutdown();

      const reader = new ObjectStoreBufReader(store, "lf.txt");
      expect(reader.size).toBe(data.length);
      expect(reader.read(4096)).toEqual(data);

      // Seek within the on-disk file.
      expect(reader.seek(6, "Start")).toBe(6);
      expect(reader.read(2).toString("utf8")).toBe("fs");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
