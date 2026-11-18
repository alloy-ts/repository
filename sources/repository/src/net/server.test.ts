import { describe, expect, test } from "vite-plus/test";
import { Server, Response, File } from "../main.ts";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import { NativeServer } from "../../../../dist/index.js";

test("native matcher preserves registration order and compiles route constraints", () => {
  const native = new NativeServer(0, "127.0.0.1", async () => "{}");
  native.addRoute("*", "/posts/*", 1, true);
  native.addRoute("GET", "/posts/:filename{.+\\.png}", 2, false, 9);
  expect(native.matchRoute("GET", "/posts/a/b.png").matches).toEqual([
    { handlerId: 1, params: {}, isMiddleware: true },
    { handlerId: 2, params: { filename: "a/b.png" }, isMiddleware: false, errorBoundaryId: 9 },
  ]);
  expect(
    native.matchRoute("POST", "/posts/a/b.png").matches.map((match) => match.handlerId),
  ).toEqual([1]);
  expect(() => native.addRoute("GET", "/broken/:id{[}", 3, false)).toThrow();
});

describe("Axum Server", () => {
  test("ordered native route matching respects strictness and optional parameters over HTTP", async () => {
    const server = new Server({
      port: 0,
      strict: false,
      routes: {
        "/animals/:type?": (request) => new Response(request.params["type"] ?? "none"),
        "/hello": () => new Response("hello"),
      },
    });
    try {
      const port = await server.start();
      const base = `http://127.0.0.1:${port}`;
      expect(await (await fetch(`${base}/hello/`)).text()).toBe("hello");
      expect(await (await fetch(`${base}/animals`)).text()).toBe("none");
      expect(await (await fetch(`${base}/animals/dog`)).text()).toBe("dog");
    } finally {
      await server.stop();
    }
  });
  test("stop cancels an idle response producer", async () => {
    let cancelled = false;
    const server = new Server({
      port: 0,
      fetch: () =>
        new Response(
          new ReadableStream({
            start(controller) {
              controller.enqueue(new Uint8Array([1]));
            },
            cancel() {
              cancelled = true;
            },
          }),
        ),
    });
    const abort = new AbortController();
    try {
      const port = await server.start();
      const response = await globalThis.fetch(`http://127.0.0.1:${port}/`, {
        signal: abort.signal,
      });
      await response.body!.getReader().read();
      await server.stop();
      expect(cancelled).toBe(true);
      expect(server.listening).toBe(false);
    } finally {
      abort.abort();
      await server.stop();
    }
  });

  test("empty chunks do not terminate the response", async () => {
    const server = new Server({
      port: 0,
      fetch: () =>
        new Response(
          new ReadableStream({
            start(controller) {
              controller.enqueue(new Uint8Array());
              controller.enqueue(new Uint8Array([0, 255, 1]));
              controller.close();
            },
          }),
        ),
    });
    try {
      const port = await server.start();
      const response = await globalThis.fetch(`http://127.0.0.1:${port}/`);
      expect(new Uint8Array(await response.arrayBuffer())).toEqual(new Uint8Array([0, 255, 1]));
    } finally {
      await server.stop();
    }
  });

  test("streams before EOF and cancels the producer on disconnect", async () => {
    let controller!: ReadableStreamDefaultController<Uint8Array>;
    let cancelled = false;
    const server = new Server({
      port: 0,
      fetch: () =>
        new Response(
          new ReadableStream({
            start(value) {
              controller = value;
              value.enqueue(new TextEncoder().encode("data: first\n\n"));
            },
            cancel() {
              cancelled = true;
            },
          }),
          { headers: { "content-type": "text/event-stream" } },
        ),
    });
    const abort = new AbortController();
    try {
      const port = await server.start();
      const response = await globalThis.fetch(`http://127.0.0.1:${port}/`, {
        signal: abort.signal,
      });
      expect(response.headers.get("content-type")).toContain("text/event-stream");
      const reader = response.body!.getReader();
      expect(new TextDecoder().decode((await reader.read()).value)).toBe("data: first\n\n");
      controller.enqueue(new TextEncoder().encode("data: second\n\n"));
      expect(new TextDecoder().decode((await reader.read()).value)).toBe("data: second\n\n");
      abort.abort();
      // The transport may observe disconnects on the next write. SSE producers normally
      // emit heartbeats; keep doing so until the transport cancels the reader.
      await expect
        .poll(() => {
          if (!cancelled) controller.enqueue(new TextEncoder().encode(": heartbeat\n\n"));
          return cancelled;
        })
        .toBe(true);
    } finally {
      abort.abort();
      await server.stop();
    }
  });

  test("HEAD cancels an unconsumed streaming body", async () => {
    let cancelled = false;
    const server = new Server({
      port: 0,
      fetch: () =>
        new Response(
          new ReadableStream({
            cancel() {
              cancelled = true;
            },
          }),
        ),
    });
    try {
      const port = await server.start();
      const response = await globalThis.fetch(`http://127.0.0.1:${port}/`, { method: "HEAD" });
      expect(response.status).toBe(200);
      expect(await response.text()).toBe("");
      await expect.poll(() => cancelled).toBe(true);
    } finally {
      await server.stop();
    }
  });

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

test("the packaged Axum server keeps a standalone Node process alive", async () => {
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

test.skipIf(process.platform === "win32")(
  "SIGTERM retains the host process default behavior",
  async () => {
    const entry = fileURLToPath(new URL("../../../../dist/index.mjs", import.meta.url));
    const child = spawn(
      process.execPath,
      [
        "--input-type=module",
        "-e",
        `
    import { Server } from ${JSON.stringify(entry)};
    setInterval(() => {}, 1000);
    const server = new Server({ port: 0 });
    console.log(await server.start());
  `,
      ],
      { stdio: ["ignore", "pipe", "pipe"] },
    );
    try {
      await Promise.race([
        once(child.stdout, "data"),
        once(child, "exit").then(() => {
          throw new Error("server exited before ready");
        }),
      ]);
      const exited = once(child, "exit");
      child.kill("SIGTERM");
      const result = await Promise.race([exited, delay(3000).then(() => "timeout")]);
      expect(result).toEqual([null, "SIGTERM"]);
    } finally {
      if (child.exitCode === null && child.signalCode === null) {
        const exited = once(child, "exit");
        child.kill("SIGKILL");
        await exited;
      }
    }
  },
);
