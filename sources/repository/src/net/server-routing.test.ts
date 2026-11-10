import { describe, expect, test } from "vite-plus/test";
import { RegExpRouter, Response, Server } from "../main.ts";

describe("Server routing", () => {
  test("text, json, params, query, headers, and a returned Response", async () => {
    const server = new Server();
    server.get("/", (c) => c.text("Hello World!"));
    server.get("/api/hello", (c) => c.json({ ok: true, message: "Hello!" }));
    server.get("/posts/:id", (c) => {
      const page = c.req.query("page");
      const id = c.req.param("id");
      c.header("X-Message", "Hi!");
      return c.text(`You want to see ${page} of ${id}`);
    });
    server.post("/posts", (c) => c.text("Created!", 201));
    server.delete("/posts/:id", (c) => c.text(`${c.req.param("id")} is deleted!`));
    server.get("/morning", () => new Response("Good morning!"));

    expect(await (await server.request("/")).text()).toBe("Hello World!");
    expect(await (await server.request("/api/hello")).json()).toEqual({
      ok: true,
      message: "Hello!",
    });
    const post = await server.request("/posts/42?page=3");
    expect(post.headers.get("x-message")).toBe("Hi!");
    expect(post.headers.get("content-type")).toContain("text/plain");
    expect(await post.text()).toBe("You want to see 3 of 42");
    const created = await server.request("/posts", { method: "POST" });
    expect(created.status).toBe(201);
    expect(await created.text()).toBe("Created!");
    expect(await (await server.request("/posts/7", { method: "DELETE" })).text()).toBe(
      "7 is deleted!",
    );
    expect(await (await server.request("/morning")).text()).toBe("Good morning!");
    expect((await server.request("/missing")).status).toBe(404);
  });

  test("methods, wildcards, all, and on()", async () => {
    const server = new Server();
    server.get("/", (c) => c.text("GET /"));
    server.post("/", (c) => c.text("POST /"));
    server.put("/", (c) => c.text("PUT /"));
    server.delete("/", (c) => c.text("DELETE /"));
    server.query("/", (c) => c.text("QUERY /"));
    server.get("/wild/*/card", (c) => c.text("GET /wild/*/card"));
    server.all("/hello", (c) => c.text("Any Method /hello"));
    server.on("PURGE", "/cache", (c) => c.text("PURGE Method /cache"));
    server.on(["PUT", "DELETE"], "/post", (c) => c.text("PUT or DELETE /post"));
    server.on("GET", ["/hello-multi", "/ja/hello", "/en/hello"], (c) => c.text("Hello"));

    expect(await (await server.request("/", { method: "PUT" })).text()).toBe("PUT /");
    const head = await server.request("/", { method: "HEAD" });
    expect(head.status).toBe(200);
    expect(await head.text()).toBe("");
    expect(await (await server.request("/", { method: "QUERY" })).text()).toBe("QUERY /");
    expect(await (await server.request("/wild/blue/card")).text()).toBe("GET /wild/*/card");
    expect((await server.request("/wild/blue/extra/card")).status).toBe(404);
    expect(await (await server.request("/hello", { method: "PATCH" })).text()).toBe(
      "Any Method /hello",
    );
    expect(await (await server.request("/cache", { method: "PURGE" })).text()).toBe(
      "PURGE Method /cache",
    );
    expect(await (await server.request("/post", { method: "DELETE" })).text()).toBe(
      "PUT or DELETE /post",
    );
    expect(await (await server.request("/ja/hello")).text()).toBe("Hello");
    expect(await (await server.request("/en/hello")).text()).toBe("Hello");
  });

  test("path, optional, and regexp params", async () => {
    const server = new Server();
    server.get("/user/:name", (c) => c.text(c.req.param("name") ?? ""));
    server.get("/posts/:id/comment/:comment_id", (c) => {
      const params = c.req.param();
      return c.text(`${params["id"]}:${params["comment_id"]}`);
    });
    server.get("/api/animal/:type?", (c) => c.text("Animal!"));
    server.get("/post/:date{[0-9]+}/:title{[a-z]+}", (c) => {
      const params = c.req.param();
      return c.text(`${params["date"]}/${params["title"]}`);
    });
    server.get("/files/:filename{.+\\.png}", (c) => c.text(c.req.param("filename") ?? ""));

    expect(await (await server.request("/user/ada")).text()).toBe("ada");
    expect(await (await server.request("/posts/4/comment/9")).text()).toBe("4:9");
    expect(await (await server.request("/api/animal")).text()).toBe("Animal!");
    expect(await (await server.request("/api/animal/cat")).text()).toBe("Animal!");
    expect(await (await server.request("/post/2024/hello")).text()).toBe("2024/hello");
    expect((await server.request("/post/year/hello")).status).toBe(404);
    expect(await (await server.request("/files/images/cat.png")).text()).toBe("images/cat.png");
  });

  test("chained routes keep the previous path", async () => {
    const server = new Server()
      .get("/endpoint", (c) => c.text("GET /endpoint"))
      .post((c) => c.text("POST /endpoint"))
      .delete((c) => c.text("DELETE /endpoint"));
    expect(await (await server.request("/endpoint")).text()).toBe("GET /endpoint");
    expect(await (await server.request("/endpoint", { method: "POST" })).text()).toBe(
      "POST /endpoint",
    );
    expect(await (await server.request("/endpoint", { method: "DELETE" })).text()).toBe(
      "DELETE /endpoint",
    );
  });

  test("route grouping, basePath, and copy-at-registration order", async () => {
    const book = new Server();
    book.get("/", (c) => c.text("List Books"));
    book.get("/:id", (c) => c.text(`Get Book: ${c.req.param("id")}`));
    book.post("/", (c) => c.text("Create Book"));
    const server = new Server();
    server.route("/book", book);
    expect(await (await server.request("/book")).text()).toBe("List Books");
    expect(await (await server.request("/book/9")).text()).toBe("Get Book: 9");
    expect(await (await server.request("/book", { method: "POST" })).text()).toBe("Create Book");

    const rooted = new Server();
    const listed = new Server();
    listed.get("/book", (c) => c.text("List Books"));
    const users = new Server().basePath("/user");
    users.get("/", (c) => c.text("List Users"));
    users.post("/", (c) => c.text("Create User"));
    rooted.route("/", listed);
    rooted.route("/", users);
    expect(await (await rooted.request("/book")).text()).toBe("List Books");
    expect(await (await rooted.request("/user", { method: "POST" })).text()).toBe("Create User");

    const api = new Server().basePath("/api");
    api.get("/book", (c) => c.text("List Books"));
    expect(await (await api.request("/api/book")).text()).toBe("List Books");

    const three = new Server();
    three.get("/hi", (c) => c.text("hi"));
    const two = new Server();
    two.route("/three", three);
    const nested = new Server();
    nested.route("/two", two);
    expect(await (await nested.request("/two/three/hi")).text()).toBe("hi");

    const threeLate = new Server();
    threeLate.get("/hi", (c) => c.text("hi"));
    const twoLate = new Server();
    const late = new Server();
    late.route("/two", twoLate);
    twoLate.route("/three", threeLate);
    expect((await late.request("/two/three/hi")).status).toBe(404);
  });

  test("registration order, fallback, middleware, notFound, and onError", async () => {
    const priority = new Server();
    priority.get("/book/a", (c) => c.text("a"));
    priority.get("/book/:slug", (c) => c.text("common"));
    expect(await (await priority.request("/book/a")).text()).toBe("a");
    expect(await (await priority.request("/book/b")).text()).toBe("common");

    const shadow = new Server();
    shadow.get("*", (c) => c.text("common"));
    shadow.get("/foo", (c) => c.text("foo"));
    expect(await (await shadow.request("/foo")).text()).toBe("common");

    const fallback = new Server();
    fallback.get("/bar", (c) => c.text("bar"));
    fallback.get("*", (c) => c.text("fallback"));
    expect(await (await fallback.request("/bar")).text()).toBe("bar");
    expect(await (await fallback.request("/foo")).text()).toBe("fallback");

    const seen: string[] = [];
    const logged = new Server();
    logged.use(async (c, next) => {
      seen.push("before");
      await next();
      c.header("X-After", "1");
      seen.push("after");
    });
    logged.get("/foo", (c) => {
      seen.push("handler");
      return c.text("foo");
    });
    const loggedResponse = await logged.request("/foo");
    expect(await loggedResponse.text()).toBe("foo");
    expect(loggedResponse.headers.get("x-after")).toBe("1");
    expect(seen).toEqual(["before", "handler", "after"]);

    const after = new Server();
    after.use(async (c, next) => {
      await next();
      c.header("x-after", "1");
    });
    after.get("/", (c) => c.text("ok"));
    const afterResponse = await after.request("/");
    expect(await afterResponse.text()).toBe("ok");
    expect(afterResponse.headers.get("x-after")).toBe("1");

    const downstream = new Server();
    downstream.onError((err, c) => c.text(`caught ${err.message}`, 500));
    downstream.use(async (_c, next) => {
      await next();
      throw new Error("after");
    });
    downstream.get("/boom", (c) => c.text("ok"));
    const thrown = await downstream.request("/boom");
    expect(thrown.status).toBe(500);
    expect(await thrown.text()).toBe("caught after");

    const missing = new Server();
    missing.notFound((c) => c.text("Custom 404 Message", 404));
    const missingResponse = await missing.request("/nope");
    expect(missingResponse.status).toBe(404);
    expect(await missingResponse.text()).toBe("Custom 404 Message");

    const child = new Server();
    child.notFound((c) => c.text("child missing", 404));
    child.get("/:id", (c) => c.text(c.req.param("id") ?? ""));
    const parent = new Server();
    parent.notFound((c) => c.text("Custom 404 Message", 404));
    parent.route("/book", child);
    expect(await (await parent.request("/book/1")).text()).toBe("1");
    expect(await (await parent.request("/absent")).text()).toBe("Custom 404 Message");

    const failing = new Server();
    failing.onError((_err, c) => c.text("Custom Error Message", 500));
    failing.get("/boom", () => {
      throw new Error("boom");
    });
    const errorResponse = await failing.request("/boom");
    expect(errorResponse.status).toBe(500);
    expect(await errorResponse.text()).toBe("Custom Error Message");

    const book = new Server();
    book.onError((_err, c) => c.text("book error", 500));
    book.get("/", () => {
      throw new Error("book");
    });
    const root = new Server();
    root.onError((_err, c) => c.text("parent error", 500));
    root.route("/book", book);
    root.get("/parent", () => {
      throw new Error("parent");
    });
    expect(await (await root.request("/book")).text()).toBe("book error");
    expect(await (await root.request("/parent")).text()).toBe("parent error");
  });

  test("strict mode, routers, getPath, env, and mount", async () => {
    const loose = new Server({ strict: false });
    loose.get("/hello", (c) => c.text("hi"));
    expect((await loose.request("/hello")).status).toBe(200);
    expect((await loose.request("/hello/")).status).toBe(200);
    const strict = new Server();
    strict.get("/hello", (c) => c.text("hi"));
    expect((await strict.request("/hello/")).status).toBe(404);

    const routed = new Server({ router: new RegExpRouter() });
    routed.get("/hello", (c) => c.text("regexp"));
    expect(await (await routed.request("/hello")).text()).toBe("regexp");

    const byHost = new Server({
      getPath: (req) => req.url.replace(/^https?:\/([^?]+).*$/, "$1"),
    });
    byHost.get("/www1.example.com/hello", (c) => c.text("hello www1"));
    byHost.get("/www2.example.com/hello", (c) => c.text("hello www2"));
    expect(await (await byHost.request("https://www1.example.com/hello")).text()).toBe(
      "hello www1",
    );
    expect(await (await byHost.request("https://www2.example.com/hello")).text()).toBe(
      "hello www2",
    );

    const byHeader = new Server({
      getPath: (req) =>
        "/" + req.headers.get("host") + req.url.replace(/^https?:\/\/[^/]+(\/[^?]*).*/, "$1"),
    });
    byHeader.get("/www1.example.com/hello", (c) => c.text("hello www1"));
    const hostResponse = await byHeader.request(
      new Request("http://www1.example.com/hello", { headers: { host: "www1.example.com" } }),
    );
    expect(await hostResponse.text()).toBe("hello www1");

    const authed = new Server();
    authed.use("/auth/*", async (c, next) => {
      c.set("user", (c.env as { TOKEN?: string } | undefined)?.["TOKEN"]);
      await next();
    });
    authed.get("/auth/me", (c) => c.text(String(c.get("user"))));
    expect(await (await authed.fetch(new Request("http://localhost/auth/me"), { TOKEN: "s" })).text()).toBe(
      "s",
    );

    const inner = new Server();
    inner.get("/hello", (c) => c.text("mounted"));
    const outer = new Server();
    outer.mount("/app", inner);
    expect(await (await outer.request("/app/hello")).text()).toBe("mounted");
  });

  test("fetch and request entry points, and fire without a service worker", async () => {
    const server = new Server();
    server.get("/hello", (c) => c.text("ok"));
    const direct = await server.fetch(new Request("http://localhost/hello"));
    expect(direct.status).toBe(200);
    expect(await direct.text()).toBe("ok");
    expect(await (await server.request(new Request("http://localhost/hello"))).text()).toBe("ok");
    const posted = new Request("http://localhost/hello", { method: "POST" });
    expect((await server.request(posted)).status).toBe(404);
  });

  test("start serves the same routes over HTTP", async () => {
    const server = new Server({ port: 0 });
    server.get("/", (c) => c.text("Hello World!"));
    server.post("/posts", (c) => c.text("Created!", 201));
    server.get("/posts/:id", (c) => {
      c.header("X-Message", "Hi!");
      return c.text(`${c.req.query("page")} of ${c.req.param("id")}`);
    });
    const port = await server.start();
    try {
      expect(server.listening).toBe(true);
      const base = `http://127.0.0.1:${port}`;
      expect(await (await fetch(`${base}/`)).text()).toBe("Hello World!");
      const created = await fetch(`${base}/posts`, { method: "POST" });
      expect(created.status).toBe(201);
      expect(await created.text()).toBe("Created!");
      const view = await fetch(`${base}/posts/9?page=2`);
      expect(view.headers.get("x-message")).toBe("Hi!");
      expect(await view.text()).toBe("2 of 9");
    } finally {
      await server.stop();
    }
    expect(server.listening).toBe(false);
  });
});
