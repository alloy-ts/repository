/**
 * Hono-style server on this branch.
 *
 * Listen (PORT defaults to 3000):
 *   node --import @oxc-node/core/register examples/server.ts
 *
 * In-process checks, no listener:
 *   EXAMPLE_SMOKE=1 node --import @oxc-node/core/register examples/server.ts
 *
 * `start()` uses Rocket. Rocket 0.5 cannot route QUERY or PURGE, so those
 * methods are exercised here with `request()` only.
 */
import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";
import { Server } from "@alloy-ts/net/server";
import { RegExpRouter } from "@alloy-ts/net/server/router/reg-exp-router";

interface AppEnv {
  Bindings: { TOKEN: string };
  Variables: { user: string };
}

export function createApp(port = Number(process.env.PORT ?? 3000)): Server<AppEnv> {
  const app = new Server<AppEnv>({
    router: new RegExpRouter(),
    port,
    host: "127.0.0.1",
  });

  // Global middleware. The header is set after the handler has responded.
  app.use(async (c, next) => {
    await next();
    c.header("x-after", "1");
  });

  // Path middleware. `c.env` is Bindings and `c.set` / `c.get` are Variables.
  app.use("/admin/*", async (c, next) => {
    if (c.env.TOKEN !== "secret") return c.text("unauthorized", 401);
    c.set("user", "ada");
    await next();
  });
  app.get("/admin/me", (c) => c.text(c.get("user")));

  app.get("/ping", (c) => c.text("pong"));
  app.get("/posts/:id", (c) => {
    const page = c.req.query("page") ?? "1";
    return c.text(`${c.req.param("id")} page ${page}`);
  });
  app.post("/posts", async (c) => {
    const body = await c.req.json<{ title: string }>();
    return c.json({ created: body.title }, 201);
  });
  app.get("/page", (c) => c.html("<p>hello</p>"));
  app.get("/go", (c) => c.redirect("/ping"));
  app.get("/status", (c) => {
    c.status(202);
    return c.text("accepted");
  });
  app.get("/raw", () => new Response("raw", { status: 200 }));

  const books = new Server<AppEnv>();
  books.get("/:id", (c) => c.text(`book ${c.req.param("id")}`));
  app.route("/books", books);

  const v2 = app.basePath("/v2");
  v2.get("/ping", (c) => c.text("pong v2"));

  app.notFound((c) => c.text("missing", 404));
  app.onError((err, c) => c.text(err.message, 500));
  app.get("/boom", () => {
    throw new Error("boom");
  });

  // In-process only. The Rocket listener answers QUERY with 400.
  app.query("/search", (c) => c.text("found"));

  return app;
}

/** Calls the routes above without opening a socket. */
export async function demonstrate(app: Server<AppEnv>): Promise<void> {
  const env = { TOKEN: "secret" };
  const ping = await app.request("/ping");
  console.log("GET /ping", ping.status, await ping.text(), ping.headers.get("x-after"));
  assert.equal(ping.status, 200);
  assert.equal(ping.headers.get("x-after"), "1");

  const postBody = await (await app.request("/posts/9?page=2")).text();
  console.log("GET /posts/9", postBody);
  assert.equal(postBody, "9 page 2");

  const created = await app.request("/posts", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ title: "note" }),
  });
  const createdBody = await created.json();
  console.log("POST /posts", created.status, createdBody);
  assert.equal(created.status, 201);
  assert.deepEqual(createdBody, { created: "note" });

  const page = await app.request("/page");
  console.log("GET /page", page.headers.get("content-type"));
  assert.match(page.headers.get("content-type") ?? "", /text\/html/);

  const redirect = await app.request("/go");
  console.log("GET /go", redirect.status, redirect.headers.get("location"));
  assert.equal(redirect.status, 302);
  assert.equal(redirect.headers.get("location"), "/ping");

  const status = await app.request("/status");
  const statusBody = await status.text();
  console.log("GET /status", status.status, statusBody);
  assert.equal(status.status, 202);
  assert.equal(statusBody, "accepted");

  const rawBody = await (await app.request("/raw")).text();
  console.log("GET /raw", rawBody);
  assert.equal(rawBody, "raw");

  const bookBody = await (await app.request("/books/7")).text();
  console.log("GET /books/7", bookBody);
  assert.equal(bookBody, "book 7");

  const v2Body = await (await app.request("/v2/ping")).text();
  console.log("GET /v2/ping", v2Body);
  assert.equal(v2Body, "pong v2");

  const meBody = await (await app.request("/admin/me", undefined, env)).text();
  console.log("GET /admin/me", meBody);
  assert.equal(meBody, "ada");

  const missing = await app.request("/nope");
  const missingBody = await missing.text();
  console.log("GET /nope", missing.status, missingBody);
  assert.equal(missing.status, 404);
  assert.equal(missingBody, "missing");

  const boom = await app.request("/boom");
  const boomBody = await boom.text();
  console.log("GET /boom", boom.status, boomBody);
  assert.equal(boom.status, 500);
  assert.equal(boomBody, "boom");

  const query = await app.request("/search", { method: "QUERY" });
  const queryBody = await query.text();
  console.log("QUERY /search", query.status, queryBody);
  assert.equal(query.status, 200);
  assert.equal(queryBody, "found");
}

const invoked = process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href;
if (invoked) {
  const app = createApp();
  await demonstrate(app);
  if (process.env.EXAMPLE_SMOKE === "1") {
    process.exit(0);
  }
  const port = await app.start();
  console.log(`listening on http://${app.host}:${port}`);
}
