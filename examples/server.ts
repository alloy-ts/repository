import { Server } from "../sources/repository/src/main.ts";

// Demonstrates the Hono-compatible Server and Context routing API in net/server.ts:
//   - HTTP methods: get, post, put, delete, query, all, on
//   - Context helpers: c.text, c.json, c.html, c.req.param, c.req.query, c.header, c.status
//   - Middleware chains with next()
//   - Grouping with route() and basePath()
//   - Error handling via onError() and customized 404 via notFound()
//   - In-process testing via server.request() and HTTP serving via server.start()

interface AppEnv {
  Bindings: {
    API_KEY: string;
  };
  Variables: {
    userId: string;
    requestStartTime: number;
  };
}

const server = new Server<AppEnv>();

// ---------------------------------------------------------------------------
// 1. Global middleware
// ---------------------------------------------------------------------------
server.use(async (c, next) => {
  c.set("requestStartTime", Date.now());
  c.header("X-Powered-By", "alloy-ts");
  await next();
  const elapsed = Date.now() - c.get("requestStartTime");
  c.header("X-Response-Time", `${elapsed}ms`);
});

// ---------------------------------------------------------------------------
// 2. Basic routing and Context helpers
// ---------------------------------------------------------------------------
server.get("/", (c) => {
  return c.text("Hello World!");
});

server.get("/api/hello", (c) => {
  return c.json({
    ok: true,
    message: "Hello!",
  });
});

// Parameterized route with query string and custom headers
server.get("/posts/:id", (c) => {
  const page = c.req.query("page") ?? "1";
  const id = c.req.param("id");
  c.header("X-Message", "Hi!");
  return c.text(`You want to see page ${page} of post ${id}`);
});

server.post("/posts", (c) => c.text("Created!", 201));
server.delete("/posts/:id", (c) => c.text(`Post ${c.req.param("id")} is deleted!`));

// HTTP QUERY method
server.query("/search", (c) => {
  const q = c.req.query("q") ?? "";
  return c.json({ query: q, results: [] });
});

// Multiple methods and paths via server.on()
server.on(["PUT", "PATCH"], "/posts/:id", (c) => {
  return c.text(`Updated post ${c.req.param("id")}`);
});

// ---------------------------------------------------------------------------
// 3. Sub-routing and basePath
// ---------------------------------------------------------------------------
const book = new Server();
book.get("/", (c) => c.text("List Books"));
book.get("/:id", (c) => c.text(`Get Book: ${c.req.param("id")}`));
book.post("/", (c) => c.text("Create Book", 201));

server.route("/books", book);

const v1 = server.basePath("/v1");
v1.get("/users", (c) => c.json([{ id: "u1", name: "Alice" }]));

// ---------------------------------------------------------------------------
// 4. Custom Error and Not Found handling
// ---------------------------------------------------------------------------
server.notFound((c) => {
  return c.text("Custom 404 - Resource Not Found", 404);
});

server.onError((err, c) => {
  console.error("Caught error:", err.message);
  return c.text(`Custom Error: ${err.message}`, 500);
});

server.get("/error-test", () => {
  throw new Error("Demonstration failure");
});

// ---------------------------------------------------------------------------
// 5. Execution: in-process request testing & native Rocket listener
// ---------------------------------------------------------------------------
async function main() {
  console.log("=== Testing server.request() in-process ===");

  const res1 = await server.request("/");
  console.log(
    "GET / ->",
    res1.status,
    await res1.text(),
    "X-Powered-By:",
    res1.headers.get("x-powered-by"),
  );

  const res2 = await server.request("/api/hello");
  console.log("GET /api/hello ->", res2.status, await res2.json());

  const res3 = await server.request("/posts/42?page=3");
  console.log("GET /posts/42?page=3 ->", res3.status, await res3.text());

  const res4 = await server.request("/books/101");
  console.log("GET /books/101 ->", res4.status, await res4.text());

  const res5 = await server.request("/v1/users");
  console.log("GET /v1/users ->", res5.status, await res5.json());

  const res6 = await server.request("/non-existent");
  console.log("GET /non-existent ->", res6.status, await res6.text());

  console.log("\n=== Starting native Rocket HTTP listener ===");
  const testServer = new Server({ port: 0 });
  testServer.get("/status", (c) => c.json({ online: true }));
  const port = await testServer.start();
  console.log(`Server listening on http://127.0.0.1:${port}`);

  try {
    const netRes = await globalThis.fetch(`http://127.0.0.1:${port}/status`);
    console.log("Network HTTP GET /status ->", netRes.status, await netRes.json());
  } finally {
    await testServer.stop();
    console.log("Server stopped successfully.");
  }
}

main().catch(console.error);

export default server;
