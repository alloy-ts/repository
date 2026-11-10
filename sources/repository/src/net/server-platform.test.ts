import { describe, expect, test } from "vite-plus/test";
import { createServer } from "node:http";
import { once } from "node:events";
import { RegExpRouter, Server } from "../main.ts";
import * as native from "../../../../dist/index.js";
import { fetch } from "../main.ts";

describe("Server platform surface", () => {
  test("RegExpRouter can be selected for a native server", async () => {
    const server = new Server({ router: new RegExpRouter() });
    server.get("/from-package", (c) => c.text("packaged"));
    expect(await (await server.request("/from-package")).text()).toBe("packaged");
    expect(server).toBeInstanceOf(Server);
  });

  test("fire() installs a Node fetch listener and warns that it is deprecated", async () => {
    const warnings: unknown[] = [];
    const original = console.warn;
    console.warn = (...args: unknown[]) => {
      warnings.push(args[0]);
    };
    try {
      const server = new Server();
      server.get("/", (c) => c.text("fired"));
      server.fire();
      expect(warnings.map(String).join(" ")).toMatch(/deprecated/);
      let pending: Promise<Response> | undefined;
      const dispatched = (
        globalThis as unknown as {
          dispatchEvent: (event: {
            type: string;
            request: Request;
            respondWith: (response: Response | Promise<Response>) => void;
          }) => boolean;
        }
      ).dispatchEvent({
        type: "fetch",
        request: new Request("http://localhost/"),
        respondWith(response) {
          pending = Promise.resolve(response);
        },
      });
      expect(dispatched).toBe(true);
      expect(await (await pending)?.text()).toBe("fired");
    } finally {
      console.warn = original;
    }
  });

  test("request() routes custom methods and the listener answers them with 400", async () => {
    const server = new Server({ port: 0 });
    server.query("/search", (c) => c.text("QUERY"));
    server.on("PURGE", "/cache", (c) => c.text("PURGE"));
    server.on("HEAD", "/both", (c) => {
      c.header("x-who", "head");
      return c.text("head-body");
    });
    server.get("/both", (c) => {
      c.header("x-who", "get");
      return c.text("get-body");
    });
    server.get("/get-only", (c) => c.text("visible"));
    server.on("FOO", "/custom", (c) => c.text(`FOO ${c.req.query("q")}`));
    expect(await (await server.request("/search", { method: "QUERY" })).text()).toBe("QUERY");
    expect(await (await server.request("/cache", { method: "PURGE" })).text()).toBe("PURGE");
    expect(await (await server.request("/custom?q=1", { method: "FOO" })).text()).toBe("FOO 1");
    const port = await server.start();
    try {
      const base = `http://127.0.0.1:${port}`;
      expect((await fetch(`${base}/search`, { method: "QUERY" })).status).toBe(400);
      expect((await fetch(`${base}/cache`, { method: "PURGE" })).status).toBe(400);
      expect((await fetch(`${base}/custom?q=1`, { method: "FOO" })).status).toBe(400);
      const head = await fetch(`${base}/both`, { method: "HEAD" });
      expect(head.headers.get("x-who")).toBe("head");
      expect(head.headers.get("content-length")).toBe(String(Buffer.byteLength("head-body")));
      expect(await head.text()).toBe("");
      const getOnly = await fetch(`${base}/get-only`, { method: "HEAD" });
      expect(getOnly.headers.get("content-length")).toBe(String(Buffer.byteLength("visible")));
      expect(await getOnly.text()).toBe("");
      expect(await (await fetch(`${base}/get-only`)).text()).toBe("visible");
    } finally {
      await server.stop();
    }
  });

  test("native request keeps duplicate headers and aborts before the socket is used", async () => {
    const first = new native.Request("http://127.0.0.1/", {
      method: "POST",
      headerList: [
        { name: "X-A", value: "1" },
        { name: "Set-Cookie", value: "a=1" },
        { name: "Set-Cookie", value: "b=2" },
      ],
      body: "kept",
    });
    const copy = first.clone();
    const replaced = new native.Request(first, { headers: { "X-B": "3" } });
    expect(await copy.text()).toBe("kept");
    expect(first.bodyUsed).toBe(true);
    expect(() => first.clone()).toThrow();
    expect(replaced.getAll("x-a")).toEqual([]);
    expect(replaced.getAll("set-cookie")).toEqual([]);
    expect(replaced.getAll("x-b")).toEqual(["3"]);
    expect(first.getAll("set-cookie")).toEqual(["a=1", "b=2"]);
    expect(first.headerEntries()).toEqual([
      ["x-a", "1"],
      ["set-cookie", "a=1"],
      ["set-cookie", "b=2"],
    ]);

    const flag = new native.AbortFlag();
    flag.abort();
    await expect(new native.Request("http://127.0.0.1:1/").start(flag)).rejects.toThrow(/abort/i);
  });

  test("fetch rejects an aborted in-flight request", async () => {
    const hanging = createServer(() => {});
    hanging.listen(0, "127.0.0.1");
    await once(hanging, "listening");
    const controller = new AbortController();
    const pending = fetch(`http://127.0.0.1:${(hanging.address() as { port: number }).port}/`, {
      signal: controller.signal,
    });
    controller.abort();
    hanging.closeAllConnections();
    try {
      await expect(pending).rejects.toThrow();
    } finally {
      await new Promise<void>((resolve) => hanging.close(() => resolve()));
    }
  });
});
