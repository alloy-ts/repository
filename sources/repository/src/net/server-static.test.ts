import { describe, expect, test } from "vite-plus/test";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createStaticServer } from "../../../../examples/server-static.ts";

describe("static local server example", () => {
  test("serves JSON and YAML files with their content types", async () => {
    const directory = await mkdtemp(join(tmpdir(), "alloy-static-"));
    await writeFile(join(directory, "data.json"), '{"name":"example","enabled":true}\n');
    await writeFile(join(directory, "data.yaml"), "name: example\nenabled: true\n");
    await writeFile(join(directory, "index.html"), "<h1>Example</h1>");
    const server = createStaticServer(directory, { port: 0 });
    try {
      const port = await server.start();
      const base = `http://127.0.0.1:${port}`;

      const json = await fetch(`${base}/data.json`);
      expect(json.status).toBe(200);
      expect(json.headers.get("content-type")).toBe("application/json");
      expect(await json.json()).toEqual({ name: "example", enabled: true });

      const yaml = await fetch(`${base}/data.yaml`);
      expect(yaml.status).toBe(200);
      expect(yaml.headers.get("content-type")).toBe("application/yaml");
      expect(await yaml.text()).toBe("name: example\nenabled: true\n");

      expect(await (await fetch(`${base}/`)).text()).toBe("<h1>Example</h1>");
      expect((await fetch(`${base}/missing.json`)).status).toBe(404);
      expect((await fetch(`${base}/data.json`, { method: "POST" })).status).toBe(405);
    } finally {
      await server.stop();
      await rm(directory, { recursive: true, force: true });
    }
  });
});
