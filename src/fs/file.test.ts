import { describe, expect, it } from "vite-plus/test";
import { File, W3cBlob, fsExistsSync } from "../main.ts";

describe("File and Blob API (src/fs)", () => {
  it("conforms to MDN and custom disk operations", async () => {
    const notreal = new File("notreal.txt");
    expect(notreal.size).toBe(0);
    expect(notreal.type).toBe("text/plain;charset=utf-8");
    expect(notreal.exists()).toBe(false);

    // URL object constructor
    const src = new File(new URL(import.meta.url));
    expect(src.exists()).toBe(true);
    expect(src.size).toBeGreaterThan(0);

    // Write text to file
    const out = new File("test_output_temp.txt");
    await out.write("It was the best of times, it was the worst of times.");
    expect(out.exists()).toBe(true);
    expect(out.size).toBe(52);
    expect(await out.text()).toBe("It was the best of times, it was the worst of times.");

    // Write binary Uint8Array / Buffer
    const encoder = new TextEncoder();
    const data = Buffer.from(encoder.encode("datadatadata"));
    await new File("test_output_temp.txt").write(data);
    expect(await out.text()).toBe("datadatadata");
    expect(await out.bytes()).toEqual(Buffer.from("datadatadata"));
    expect(await out.arrayBuffer()).toEqual(Buffer.from("datadatadata"));

    // Delete file
    out.delete();
    expect(out.exists()).toBe(false);

    // JSON file reading
    const jsonFile = new File("test_json_temp.json");
    await jsonFile.write(JSON.stringify({ greeting: "hello", count: 42 }));
    expect(await jsonFile.json()).toEqual({ greeting: "hello", count: 42 });
    jsonFile.delete();

    // Custom MIME type override using options
    const custom = new File("custom.txt", {
      headers: { "content-type": "application/custom-data" },
    });
    expect(custom.type).toBe("application/custom-data");

    // Recursive directory deletion
    const nestedFile = new File("temp_test_dir/nested.txt");
    await nestedFile.write("nested content");
    expect(fsExistsSync("temp_test_dir/nested.txt")).toBe(true);
    new File("temp_test_dir").delete({ recursive: true });
    expect(fsExistsSync("temp_test_dir")).toBe(false);
  });

  it("supports W3cBlob operations", async () => {
    const blob = new W3cBlob(["hello ", Buffer.from("world")], {
      type: "text/plain",
    });
    expect(blob.size).toBe(11);
    expect(blob.type).toBe("text/plain");
    expect(await blob.text()).toBe("hello world");

    const slice = blob.slice(0, 5);
    expect(await slice.text()).toBe("hello");
    expect(slice.size).toBe(5);
  });
});
