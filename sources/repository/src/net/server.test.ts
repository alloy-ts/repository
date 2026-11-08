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
  test("routing methods, context helpers, and middleware matching Hono style Server API", async () => {
    const server = new Server({ port: 0 });

    server.get("/", (c) => {
      return c.text("Hello World!");
    });

    server.get("/api/hello", (c) => {
      return c.json({
        ok: true,
        message: "Hello!",
      });
    });

    server.get("/posts/:id", (c) => {
      const page = c.req.query("page");
      const id = c.req.param("id");
      c.header("X-Message", "Hi!");
      return c.text(`You want to see ${page} of ${id}`);
    });

    server.post("/posts", (c) => c.text("Created!", 201));
    server.delete("/posts/:id", (c) => c.text(`${c.req.param("id")} is deleted!`));

    // Test c.req methods
    server.get("/req-test", (c) => {
      return c.json({
        method: c.req.method,
        url: c.req.url,
        header: c.req.header("x-custom"),
        allHeaders: typeof c.req.header(),
        allQuery: c.req.query(),
      });
    });

    // Test server.request()
    const res1 = await server.request("/");
    expect(res1.status).toBe(200);
    expect(await res1.text()).toBe("Hello World!");

    const res2 = await server.request("/api/hello");
    expect(res2.status).toBe(200);
    expect(await res2.json()).toEqual({ ok: true, message: "Hello!" });
    expect(res2.headers.get("content-type")).toContain("application/json");

    const res3 = await server.request("/posts/123?page=5");
    expect(res3.status).toBe(200);
    expect(res3.headers.get("x-message")).toBe("Hi!");
    expect(await res3.text()).toBe("You want to see 5 of 123");

    const res4 = await server.request("/posts", { method: "POST" });
    expect(res4.status).toBe(201);
    expect(await res4.text()).toBe("Created!");

    const res5 = await server.request(
      new Request("http://localhost/posts/abc", { method: "DELETE" }),
    );
    expect(res5.status).toBe(200);
    expect(await res5.text()).toBe("abc is deleted!");

    const resReq = await server.request("/req-test?a=1&b=2", {
      headers: { "x-custom": "tested" },
    });
    expect(await resReq.json()).toEqual({
      method: "GET",
      url: "http://localhost/req-test?a=1&b=2",
      header: "tested",
      allHeaders: "object",
      allQuery: { a: "1", b: "2" },
    });

    // Over HTTP via RocketServer
    try {
      const port = await server.start();
      const base = `http://127.0.0.1:${port}`;

      const httpRes1 = await globalThis.fetch(`${base}/`);
      expect(await httpRes1.text()).toBe("Hello World!");

      const httpRes2 = await globalThis.fetch(`${base}/posts/99?page=3`);
      expect(httpRes2.headers.get("x-message")).toBe("Hi!");
      expect(await httpRes2.text()).toBe("You want to see 3 of 99");

      const httpRes3 = await globalThis.fetch(`${base}/posts`, { method: "POST" });
      expect(httpRes3.status).toBe(201);
      expect(await httpRes3.text()).toBe("Created!");
    } finally {
      await server.stop();
    }
  });

  test("middleware, routing hierarchy, notFound, onError, strict mode and mount", async () => {
    const server = new Server();

    // Middleware
    server.use("/admin/*", async (c, next) => {
      c.set("admin", true);
      c.header("X-Admin", "true");
      await next();
    });

    server.get("/admin/dashboard", (c) => {
      return c.text(`Admin status: ${c.get("admin")}`);
    });

    // Sub-route
    const sub = new Server();
    sub.get("/item", (c) => c.text("sub item"));
    sub.onError((err, c) => c.text(`Sub error: ${err.message}`, 500));
    sub.get("/crash", () => {
      throw new Error("sub exploded");
    });
    server.route("/api", sub);

    // basePath
    const v1 = server.basePath("/v1");
    v1.get("/users", (c) => c.text("v1 users"));

    // notFound and onError
    server.notFound((c) => c.text("Custom 404", 404));
    server.onError((err, c) => c.text(`Handled: ${err.message}`, 500));
    server.get("/throw", () => {
      throw new Error("top exploded");
    });

    // mount
    server.mount("/mounted", (req) => new Response(`Mounted: ${new URL(req.url).pathname}`));

    // Test middleware
    const adminRes = await server.request("/admin/dashboard");
    expect(adminRes.status).toBe(200);
    expect(adminRes.headers.get("x-admin")).toBe("true");
    expect(await adminRes.text()).toBe("Admin status: true");

    // Test sub-route
    const subRes = await server.request("/api/item");
    expect(await subRes.text()).toBe("sub item");

    // Test sub-route onError priority
    const subErrRes = await server.request("/api/crash");
    expect(subErrRes.status).toBe(500);
    expect(await subErrRes.text()).toBe("Sub error: sub exploded");

    // Test top-level onError
    const topErrRes = await server.request("/throw");
    expect(topErrRes.status).toBe(500);
    expect(await topErrRes.text()).toBe("Handled: top exploded");

    // Test basePath
    const v1Res = await server.request("/v1/users");
    expect(await v1Res.text()).toBe("v1 users");

    // Test notFound
    const notFoundRes = await server.request("/does-not-exist");
    expect(notFoundRes.status).toBe(404);
    expect(await notFoundRes.text()).toBe("Custom 404");

    // Test mount
    const mountedRes = await server.request("/mounted/hello/world");
    expect(await mountedRes.text()).toBe("Mounted: /hello/world");

    // Test strict mode
    const strictServer = new Server({ strict: true });
    strictServer.get("/strict", (c) => c.text("strict"));
    expect((await strictServer.request("/strict")).status).toBe(200);
    expect((await strictServer.request("/strict/")).status).toBe(404);

    const looseServer = new Server({ strict: false });
    looseServer.get("/loose", (c) => c.text("loose"));
    expect((await looseServer.request("/loose")).status).toBe(200);
    expect((await looseServer.request("/loose/")).status).toBe(200);
  });

  test("advanced routing: query method, chained routes, regex, optional params, getPath and grouping order", async () => {
    const server = new Server();

    // query method
    server.query("/", (c) => c.text("QUERY /"));

    // wildcard
    server.get("/wild/*/card", (c) => c.text("GET /wild/*/card"));

    // multiple method & multiple path
    server.on(["PUT", "DELETE"], "/post-multi", (c) => c.text("PUT or DELETE /post-multi"));
    server.on("GET", ["/hello-a", "/hello-b"], (c) => c.text("Hello"));

    // path parameter all
    server.get("/posts/:id/comment/:comment_id", (c) => {
      const { id, comment_id } = c.req.param();
      return c.json({ id, comment_id });
    });

    // optional parameter
    server.get("/api/animal/:type?", (c) => c.text(c.req.param("type") ?? "Animal!"));

    // regex parameter
    server.get("/post/:date{[0-9]+}/:title{[a-z]+}", (c) => {
      const { date, title } = c.req.param();
      return c.json({ date, title });
    });

    // regex parameter including slashes
    server.get("/files/:filename{.+\\.png}", (c) => {
      return c.text(c.req.param("filename")!);
    });

    // chained route
    server
      .get("/endpoint", (c) => c.text("GET /endpoint"))
      .post((c) => c.text("POST /endpoint"))
      .delete((c) => c.text("DELETE /endpoint"));

    // test query method
    const queryRes = await server.request("/", { method: "QUERY" });
    expect(await queryRes.text()).toBe("QUERY /");

    // test wildcard
    const wildRes = await server.request("/wild/foo/card");
    expect(await wildRes.text()).toBe("GET /wild/*/card");

    // test multi methods and paths
    const putRes = await server.request("/post-multi", { method: "PUT" });
    expect(await putRes.text()).toBe("PUT or DELETE /post-multi");
    const delRes = await server.request("/post-multi", { method: "DELETE" });
    expect(await delRes.text()).toBe("PUT or DELETE /post-multi");
    const helloARes = await server.request("/hello-a");
    expect(await helloARes.text()).toBe("Hello");
    const helloBRes = await server.request("/hello-b");
    expect(await helloBRes.text()).toBe("Hello");

    // test path parameter all
    const commentRes = await server.request("/posts/12/comment/34");
    expect(await commentRes.json()).toEqual({ id: "12", comment_id: "34" });

    // test optional parameter
    const anim1 = await server.request("/api/animal");
    expect(await anim1.text()).toBe("Animal!");
    const anim2 = await server.request("/api/animal/dog");
    expect(await anim2.text()).toBe("dog");

    // test regex parameter
    const dateTitleRes = await server.request("/post/20260101/update");
    expect(await dateTitleRes.json()).toEqual({ date: "20260101", title: "update" });
    const invalidDateRes = await server.request("/post/abc/update");
    expect(invalidDateRes.status).toBe(404);

    // test regex including slashes
    const fileRes = await server.request("/files/images/sub/photo.png");
    expect(await fileRes.text()).toBe("images/sub/photo.png");

    // test chained route
    expect(await (await server.request("/endpoint")).text()).toBe("GET /endpoint");
    expect(await (await server.request("/endpoint", { method: "POST" })).text()).toBe(
      "POST /endpoint",
    );
    expect(await (await server.request("/endpoint", { method: "DELETE" })).text()).toBe(
      "DELETE /endpoint",
    );

    // test getPath with host header routing
    const hostServer = new Server({
      getPath: (req) =>
        "/" + req.headers.get("host") + req.url.replace(/^https?:\/\/[^/]+(\/[^?]*).*/, "$1"),
    });
    hostServer.get("/www1.example.com/hello", (c) => c.text("hello www1"));
    const hostReq = new Request("http://localhost/hello", {
      headers: { host: "www1.example.com" },
    });
    expect(await (await hostServer.request(hostReq)).text()).toBe("hello www1");

    // test grouping ordering
    const three = new Server();
    const two = new Server();
    const root = new Server();
    three.get("/hi", (c) => c.text("hi"));
    two.route("/three", three);
    root.route("/two", two);
    expect(await (await root.request("/two/three/hi")).text()).toBe("hi");

    const badTwo = new Server();
    const badRoot = new Server();
    badRoot.route("/two", badTwo);
    badTwo.route("/three", three);
    expect((await badRoot.request("/two/three/hi")).status).toBe(404);
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
