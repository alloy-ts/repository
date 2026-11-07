import { afterEach, beforeEach, describe, expect, test } from "vite-plus/test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { File, Blob, Response, Object as GitObject } from "../main.ts";
let dir: string;
beforeEach(async () => {
  dir = await mkdtemp(join(tmpdir(), "alloy-file-"));
});
afterEach(async () => {
  await rm(dir, { recursive: true, force: true });
});

describe("lazy File and Web Blob", () => {
  test("nonexistent paths, RequestOpts MIME, typed-array writes and metadata", async () => {
    const file = new File(join(dir, "output.txt"));
    expect(file).toBeInstanceOf(globalThis.File);
    expect(file).toBeInstanceOf(Blob);
    expect(file.size).toBe(0);
    expect(file.type).toBe("text/plain;charset=utf-8");
    expect(await file.exists()).toBe(false);
    expect(await file.write(new TextEncoder().encode("datadatadata"))).toBe(12);
    expect(file.name).toBe("output.txt");
    expect(file.lastModified).toBeGreaterThan(0);
    expect(await file.text()).toBe("datadatadata");
    expect(await new Response(file).text()).toBe("datadatadata");
    expect(await file.exists()).toBe(true);
    expect(await file.arrayBuffer()).toBeInstanceOf(ArrayBuffer);
    expect(await file.bytes()).toBeInstanceOf(Uint8Array);
    expect(
      new File(join(dir, "output.txt"), {
        headers: new Headers({ "Content-Type": "application/json" }),
      }).type,
    ).toBe("application/json");
    await file.write('{"ok":true}');
    expect(await file.json()).toEqual({ ok: true });
    file.delete();
    expect(await file.exists()).toBe(false);
  });
  test("file URLs decode spaces, percent signs and unicode; invalid schemes reject", async () => {
    const path = join(dir, "a space % café.txt");
    const url = pathToFileURL(path);
    const file = new File(url);
    await file.write("url");
    expect(await new File(url.href).text()).toBe("url");
    expect(() => new File(new URL("https://example.com/file"))).toThrow();
  });
  test("incremental readable file stream, slice, and recursive deletion", async () => {
    const file = new File(join(dir, "nested", "large.bin"));
    const data = new Uint8Array(200_000).fill(42);
    await file.write(data);
    const reader = file.stream().getReader();
    let bytes = 0,
      chunks = 0;
    while (true) {
      const item = await reader.read();
      if (item.done) break;
      bytes += item.value.length;
      chunks++;
    }
    expect(bytes).toBe(data.length);
    expect(chunks).toBeGreaterThan(1);
    expect(await file.slice(-3).bytes()).toEqual(new Uint8Array([42, 42, 42]));
    new File(join(dir, "nested")).delete({ recursive: true });
    expect(await file.exists()).toBe(false);
  });
  test("Blob parts, slice defaults, streams and ArrayBuffer", async () => {
    const blob = new Blob(["hello", new Uint8Array([33])], { type: "TEXT/PLAIN" });
    expect(blob.type).toBe("text/plain");
    expect(blob.size).toBe(6);
    expect(blob.slice(0, 2).type).toBe("");
    expect(await blob.slice(-1).text()).toBe("!");
    expect(await blob.arrayBuffer()).toBeInstanceOf(ArrayBuffer);
    expect(await new globalThis.Response(blob.stream()).text()).toBe("hello!");
    expect(typeof (GitObject as any).Blob).toBe("function");
  });
});

test("native File and Blob return real Web streams", async () => {
  const native = await import("../../../../dist/index.js");
  const path = join(dir, "native.bin");
  const file = new native.File(path);
  await file.write("native stream");
  expect(await new globalThis.Response(file.stream()).text()).toBe("native stream");
  const blob = new native.Blob(["native blob"]);
  expect(await new globalThis.Response(blob.stream()).text()).toBe("native blob");
});
