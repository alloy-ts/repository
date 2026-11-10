import { describe, expect, test } from "vite-plus/test";
import { Response } from "./response.ts";
import { Server } from "../main.ts";

describe("Context and request helpers", () => {
  test("c.notFound uses the registered handler, including under middleware and a raw Response", async () => {
    const server = new Server();
    server.notFound((c) => c.text("custom", 404));
    server.get("/u/:id", (c) => c.notFound());
    const missing = await server.request("/u/9");
    expect(missing.status).toBe(404);
    expect(await missing.text()).toBe("custom");

    const raw = new Server();
    raw.notFound(() => new Response("raw", { status: 404, headers: { "x-raw": "1" } }));
    raw.get("/u/:id", (c) => c.notFound());
    const rawResponse = await raw.request("/u/9");
    expect(rawResponse.status).toBe(404);
    expect(rawResponse.headers.get("x-raw")).toBe("1");
    expect(await rawResponse.text()).toBe("raw");

    const wrapped = new Server();
    wrapped.notFound((c) => c.text("custom", 404));
    wrapped.use(async (_c, next) => {
      await next();
    });
    wrapped.get("/u/:id", (c) => c.notFound());
    const wrappedResponse = await wrapped.request("/u/9");
    expect(wrappedResponse.status).toBe(404);
    expect(await wrappedResponse.text()).toBe("custom");
  });

  test("html, body, redirect, status, and appended headers", async () => {
    const server = new Server();
    server.get("/html", (c) => c.html("<p>hi</p>", 201));
    server.get("/body", (c) => c.body("plain", 202, { "x-body": "yes" }));
    server.get("/go", (c) => {
      c.header("x-from", "app");
      return c.redirect("/next", 307);
    });
    server.use("/status/*", async (c, next) => {
      await next();
      c.status(201);
      c.header("x-appended", "part1");
      c.header("x-appended", "part2", { append: true });
    });
    server.get("/status/ok", (c) => c.text("ok"));

    const html = await server.request("/html");
    expect(html.status).toBe(201);
    expect(html.headers.get("content-type")).toContain("text/html");
    expect(await html.text()).toBe("<p>hi</p>");

    const body = await server.request("/body");
    expect(body.status).toBe(202);
    expect(body.headers.get("x-body")).toBe("yes");
    expect(await body.text()).toBe("plain");

    const redirect = await server.request("/go");
    expect(redirect.status).toBe(307);
    expect(redirect.headers.get("location")).toBe("/next");
    expect(redirect.headers.get("x-from")).toBe("app");

    const status = await server.request("/status/ok");
    expect(status.status).toBe(201);
    expect(await status.text()).toBe("ok");
    expect(status.headers.get("x-appended")).toBe("part1, part2");
  });

  test("c.req body helpers, header() and query() without a name, and executionCtx", async () => {
    const server = new Server();
    server.post("/text", async (c) => c.text(await c.req.text()));
    server.post("/json", async (c) => c.json(await c.req.json()));
    server.post("/form", async (c) => {
      const value = (await c.req.formData()).get("a");
      return c.text(typeof value === "string" ? value : "");
    });
    server.get("/look", (c) => {
      const query = c.req.query();
      const headers = c.req.header();
      return c.json({ page: query["page"], host: headers["x-host"] });
    });
    let seen: unknown;
    server.get("/ctx", (c) => {
      seen = c.executionCtx;
      return c.text("ctx");
    });

    expect(await (await server.request("/text", { method: "POST", body: "hello" })).text()).toBe(
      "hello",
    );
    expect(await (await server.request("/json", { method: "POST", body: '{"n":1}' })).json()).toEqual({
      n: 1,
    });
    expect(
      await (
        await server.request("/form", {
          method: "POST",
          headers: { "content-type": "application/x-www-form-urlencoded" },
          body: "a=bee",
        })
      ).text(),
    ).toBe("bee");
    const looked = await server.request("/look?page=3", { headers: { "x-host": "app" } });
    expect(await looked.json()).toEqual({ page: "3", host: "app" });

    const marker = { kind: "execution" };
    await server.fetch(new Request("http://localhost/ctx"), undefined, marker);
    expect(seen).toBe(marker);
    seen = undefined;
    await server.request("/ctx", undefined, undefined, marker);
    expect(seen).toBe(marker);
  });

  test("c.req.query decodes + as a space and keeps %2B and path plus signs", async () => {
    const server = new Server();
    server.get("/search", (c) => {
      const all = c.req.query();
      return c.json({
        q: c.req.query("q"),
        x: c.req.query("x"),
        key: all["a b"],
      });
    });
    server.get("/files/:name", (c) => c.text(c.req.param("name") ?? ""));

    const found = await server.request("/search?q=hello+world&a+b=c&x=1%2B1");
    expect(await found.json()).toEqual({ q: "hello world", x: "1+1", key: "c" });
    expect(await (await server.request("/files/hello+world")).text()).toBe("hello+world");
  });

  test("patch, options, and head are first-class methods", async () => {
    const server = new Server();
    server.patch("/item", (c) => c.text("patched"));
    server.options("/item", (c) => c.text("options"));
    server.head("/item", (c) => {
      c.header("x-who", "head");
      return c.text("head-body");
    });
    server.get("/item", (c) => {
      c.header("x-who", "get");
      return c.text("get-body");
    });
    expect(await (await server.request("/item", { method: "PATCH" })).text()).toBe("patched");
    expect(await (await server.request("/item", { method: "OPTIONS" })).text()).toBe("options");
    const head = await server.request("/item", { method: "HEAD" });
    expect(head.status).toBe(200);
    expect(head.headers.get("x-who")).toBe("head");
    expect(head.headers.get("content-length")).toBe(String(Buffer.byteLength("head-body")));
    expect(await head.text()).toBe("");
    const get = await server.request("/item");
    expect(get.headers.get("x-who")).toBe("get");
    expect(await get.text()).toBe("get-body");
  });
});
