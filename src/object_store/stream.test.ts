import { describe, expect, test } from "vite-plus/test";
import { StreamObjectStore } from "../main.ts";

describe("StreamObjectStore (src/object_store/stream.rs)", () => {
  test("constructs StreamObjectStore and reports default chunk size and path", () => {
    const store = new StreamObjectStore(4096);
    expect(store.chunkSize).toBe(4096);
    expect(store.path).toContain("stream");
    expect(store.objectHash).toBe("sha1");
    expect(store.objectFormat).toBe("sha1");
  });

  test("writeObject and read back object content", () => {
    const store = new StreamObjectStore(1024);
    const data = Buffer.from("stream-store-content\n");
    const id = store.writeObject("blob", data);
    expect(id).toMatch(/^[0-9a-f]{40}$/);
    expect(store.contains(id)).toBe(true);

    const found = store.tryFind(id);
    expect(found).not.toBeNull();
    expect(found!.kind).toBe("blob");
    expect(Buffer.from(found!.data).equals(data)).toBe(true);

    const header = store.tryHeader(id);
    expect(header).not.toBeNull();
    expect(header!.kind).toBe("blob");
    expect(header!.size).toBe(data.length);
  });

  test("reader streams content in chunks", () => {
    const store = new StreamObjectStore(512);
    const data = Buffer.from("streaming-reader-test\n");
    const id = store.writeObject("blob", data);
    const reader = store.reader(id);
    expect(reader.kind).toBe("blob");
    expect(reader.size).toBe(data.length);

    const chunks: Buffer[] = [];
    let c = reader.read(8);
    while (c.length > 0) {
      chunks.push(Buffer.from(c));
      c = reader.read(8);
    }
    expect(Buffer.concat(chunks).equals(data)).toBe(true);
  });

  test("writer buffers and finalizes object", () => {
    const store = new StreamObjectStore(2048);
    const data = Buffer.from("writer-test-stream\n");
    const w = store.writer("blob", data.length);
    w.write(data);
    const id = w.finalize();
    expect(store.contains(id)).toBe(true);
    expect(Buffer.from(store.tryFind(id)!.data).equals(data)).toBe(true);
  });
});
