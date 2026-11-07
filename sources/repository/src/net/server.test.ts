import { describe, expect, test } from "vite-plus/test";
import { Server, Response, File } from "../main.ts";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { join } from "node:path";

describe("Rocket Server", () => {
  test("static, dynamic, async method, wildcard, redirect, file and fallback routes over HTTP", async () => {
    const dir = await mkdtemp(join(tmpdir(), "alloy-server-"));
    const icon = join(dir, "favicon.ico");
    await writeFile(icon, new Uint8Array([0, 255, 1]));
    const server = new Server({
      port: 0,
      routes: {
        "/api/status": new Response("healthy", { status: 201, headers: { "x-test": "static" } }),
        "/users/:id": (req) => new Response(`Hello User ${req.params["id"]}!`),
        "/api/posts": {
          GET: () => new Response("List posts"),
          POST: async (req) =>
            Response.json({ created: true, ...((await req.json()) as Record<string, unknown>) }),
        },
        "/api/*": Response.json({ message: "Not found" }, { status: 404 }),
        "/blog/hello": Response.redirect("/blog/hello/world"),
        "/favicon.ico": new File(icon, { type: "image/x-icon" }),
        "/bad": () => {
          throw new Error("handler failure");
        },
      },
      fetch: (req) => new Response(new URL(req.url).pathname, { status: 404 }),
    });
    try {
      const port = await server.start();
      expect(port).toBeGreaterThan(0);
      expect(server.listening).toBe(true);
      const base = `http://127.0.0.1:${port}`;
      const status = await globalThis.fetch(`${base}/api/status`);
      expect(status.status).toBe(201);
      expect(status.headers.get("x-test")).toBe("static");
      expect(await status.text()).toBe("healthy");
      expect(await (await globalThis.fetch(`${base}/api/status`)).text()).toBe("healthy");
      expect(await (await globalThis.fetch(`${base}/users/a%20b`)).text()).toBe("Hello User a b!");
      expect(
        await (
          await globalThis.fetch(`${base}/api/posts`, {
            method: "POST",
            body: JSON.stringify({ title: "post" }),
          })
        ).json(),
      ).toEqual({ created: true, title: "post" });
      expect((await globalThis.fetch(`${base}/api/unknown`)).status).toBe(404);
      expect(
        (await globalThis.fetch(`${base}/blog/hello`, { redirect: "manual" })).headers.get(
          "location",
        ),
      ).toBe("/blog/hello/world");
      expect(
        new Uint8Array(await (await globalThis.fetch(`${base}/favicon.ico`)).arrayBuffer()),
      ).toEqual(new Uint8Array([0, 255, 1]));
      expect(await (await globalThis.fetch(`${base}/missing`)).text()).toBe("/missing");
      expect((await globalThis.fetch(`${base}/bad`)).status).toBe(500);
      await expect(server.start()).rejects.toThrow();
    } finally {
      await server.stop();
      await rm(dir, { recursive: true, force: true });
    }
    expect(server.listening).toBe(false);
    await server.serve();
    await server.stop();
  });
  test("bind failure rejects and stop releases the port", async () => {
    const first = new Server({ port: 0 });
    await first.start();
    const second = new Server({ port: first.port });
    try {
      await expect(second.start()).rejects.toThrow();
    } finally {
      await first.stop();
      await second.stop();
    }
    await second.start();
    await second.stop();
  });
});

test("the packaged Rocket server keeps a standalone Node process alive", async () => {
  const entry = fileURLToPath(new URL("../../../../dist/index.mjs", import.meta.url));
  const script = `import { Server, Response } from ${JSON.stringify(entry)};
    const server = new Server({ port: 0, routes: { "/": new Response("alive") } });
    console.log(await server.start());`;
  const child = spawn(process.execPath, ["--input-type=module", "-e", script], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  try {
    const [output] = await Promise.race([
      once(child.stdout, "data"),
      once(child, "exit").then(([code]) => {
        throw new Error(`server exited before listening: ${code}`);
      }),
    ]);
    const port = Number(output.toString().trim());
    await delay(100);
    expect(child.exitCode).toBeNull();
    expect(await (await globalThis.fetch(`http://127.0.0.1:${port}/`)).text()).toBe("alive");
  } finally {
    if (child.exitCode === null) {
      child.kill("SIGKILL");
      await once(child, "exit");
    }
  }
});
