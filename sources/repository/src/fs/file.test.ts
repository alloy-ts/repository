import { describe, expect, test } from "vite-plus/test";
import { File } from "../main.ts";

describe("File (src/fs/file.rs)", () => {
  test("File pointing to nonexistent file", async () => {
    const notreal = new File("notreal.txt");
    expect(notreal.size).toBe(0);
    expect(notreal.type).toBe("text/plain;charset=utf-8");
    const exists = await notreal.exists();
    expect(exists).toBe(false);
  });

  test("File write, read, stream, arrayBuffer and delete", async () => {
    const foo = new File("foo_test.txt");
    await foo.write("Hello World File");
    expect(await foo.exists()).toBe(true);
    expect(foo.size).toBe(16);
    expect(await foo.text()).toBe("Hello World File");

    const bytes = await foo.bytes();
    expect(bytes.length).toBe(16);

    const buf = await foo.arrayBuffer();
    expect(buf.length).toBe(16);

    const stream = await foo.stream();
    expect(stream.length).toBe(16);

    foo.delete();
    expect(await foo.exists()).toBe(false);
  });

  test("File initialized with URL object", async () => {
    const src = new File(new URL(import.meta.url));
    expect(await src.exists()).toBe(true);
    expect(src.size).toBeGreaterThan(0);
  });

  test("File write with string and Uint8Array", async () => {
    const out = new File("output.txt");
    await out.write(`It was the best of times, it was the worst of times.`);
    expect(await out.text()).toBe(`It was the best of times, it was the worst of times.`);

    const encoder = new TextEncoder();
    const data = encoder.encode("datadatadata"); // Uint8Array
    await new File("output.txt").write(Buffer.from(data));
    expect(await new File("output.txt").text()).toBe("datadatadata");

    new File("output.txt").delete();
    expect(await new File("output.txt").exists()).toBe(false);
  });

  test("File recursive delete", async () => {
    const dir = new File("test_dir_to_delete");
    await new File("test_dir_to_delete/subfile.txt").write("hello");
    expect(await new File("test_dir_to_delete/subfile.txt").exists()).toBe(true);

    new File("test_dir_to_delete").delete({ recursive: true });
    expect(await dir.exists()).toBe(false);
  });
});

test("Web Blob works as expected and Object.Blob exists", async () => {
  const { Blob, Object: GitObject } = await import("../main.ts");
  const b = new Blob(["test blob"], { type: "text/plain" });
  expect(b.size).toBe(9);
  expect(b.type).toBe("text/plain");
  expect(await b.text()).toBe("test blob");
  expect(typeof (GitObject as any).Blob).toBe("function");
});
