# Server manual

This manual describes the server APIs currently implemented in this repository.
The Node entry uses an Axum HTTP listener through a Rust N-API addon. The
`repository/cloud` entry contains a Web `Request` → `Response` application and
adapters that can run without importing the native addon.

## Build and run locally

Use Node.js 24, npm 11.11.0, and a Rust toolchain with Cargo. From the repository
root:

```sh
npm install
npm run build
node --import @oxc-node/core/register examples/server.ts
```

The example listens on `127.0.0.1:8080`. Set `PORT` and `HOST` to change the
listener; `PORT=0` asks the OS to choose an available port and prints the chosen
address. In a second terminal:

```sh
curl http://127.0.0.1:8080/health
curl http://127.0.0.1:8080/users/alice
curl -X POST --data-binary 'hello' http://127.0.0.1:8080/echo
```

Press Ctrl+C to stop the example. The server also handles SIGTERM. The native
addon is required to run this local listener; build before launching an example
from a fresh checkout.

## Local `Server` API

Import `Server` and `Response` from `repository` (or, inside this checkout, from
`sources/repository/src/main.ts`):

```ts
import { Server, Response } from "repository";

const server = new Server({
  host: "127.0.0.1", // default
  port: 0,           // default is 3000
  strict: true,      // default: /hello and /hello/ differ
  routes: {
    "/health": () => Response.json({ ok: true }),
    "/users/:id": (request) => Response.json({ id: request.params.id }),
    "/items": {
      GET: () => Response.json({ items: [] }),
      POST: async (request) => Response.json(await request.json(), { status: 201 }),
    },
  },
  fetch: () => new Response("Not Found", { status: 404 }),
});

const port = await server.start();
console.log(`Listening on ${port}`);
// Later, for example on a process shutdown signal:
await server.stop();
```

`start()` resolves after the socket binds and returns the actual port. `serve()`
is an alias. `port` and `listening` are readable properties. Calling `start()`
while it is already running rejects; `stop()` is safe to call again. A failed
bind rejects. The listener uses HTTP and an IP address literal for `host`; it
does not provide TLS or WebSocket upgrades.

The route table is compiled in Rust when `Server` is constructed. Routes match
in the insertion order of the `routes` object. Supported patterns include
static paths (`/health`), named parameters (`/users/:id`), optional parameters
(`/animals/:type?`), `*` wildcards, and regex constraints such as
`/posts/:date{[0-9]+}`. Values are available in `request.params`. A route may
be a `Response`, a file, a function, or a method-to-handler object. A `HEAD`
request can use a route's `GET` handler and receives no body. If nothing
matches, the `fetch` fallback runs; without it the server returns 404. Handler
errors become 500 responses.

Request bodies are buffered up to 16 MiB by the native transport. Response
bodies can stream; a `ReadableStream` or a file route does not need to finish
before headers are sent. On `stop()`, active JavaScript readers are cancelled.
For an idle SSE stream, send periodic heartbeats if prompt client-disconnect
cleanup matters. The server owns HTTP framing headers such as Content-Length.

The native `NativeServer` binding also exposes `addRoute` and `matchRoute` for
ordered pattern matching. Application code should normally use the public
`Server` wrapper so JavaScript handlers and response streams remain on the JS
thread. Fluent `.get()`/`.post()`, onion middleware, context helpers, and
sub-app mounting from the proposed API design are **not implemented yet**.

### Static local files

Run `node --import @oxc-node/core/register examples/server-static.ts` to serve
the files in `examples/static` on port 8080. Pass a directory as the first
argument to serve another folder; `PORT` and `HOST` override the listener.
The example streams files and sets `application/json` for `.json` and
`application/yaml` for `.yaml` and `.yml`.

### MCP routes in a Rust host

Build with `cargo build -p repository --features mcp` to include
`sources/repository/src/net/server_mcp.rs` and its optional `tower-mcp`
dependency. Build a `tower_mcp::McpRouter` with tools, resources, or prompts,
then wrap it with `McpServer::new(router)`. The resulting `.into_router()` is
an ordinary `axum::Router`: streamable HTTP is mounted at `/mcp` and WebSocket
at `/mcp/ws`. A Rust host can pass it to `Server::start_with_router(...)`,
call `Server::start_with_mcp(...)`, or merge it into any other Axum app. The
TypeScript `Server.start()` path above does not install MCP routes.

Use `ToolBuilder::layer(...)` for one tool, `McpServer::http(...)` and
`McpServer::websocket(...)` for transport-wide Tower middleware, and
`McpServer::into_router().layer(...)` for HTTP request middleware. The
transport configuration callbacks receive the corresponding tower-mcp
transport, so session and origin settings can be set there too. For schema
derives, import `tower_mcp::schemars::JsonSchema` to match tower-mcp's
version of schemars.

## Portable application and function adapters

`repository/cloud` exports `createApp`, `cloudflare`, `lambda`, `google`,
`azure`, and `requireCapabilities`. Import this entry for cloud targets; the
package root loads native code. A portable app uses standard Web APIs:

```ts
import { createApp, lambda } from "repository/cloud";

export const app = createApp({
  routes: {
    "/health": () => Response.json({ ok: true }),
    "/echo": { POST: async (request) => new Response(await request.arrayBuffer()) },
  },
});

export const handler = lambda(app);
```

`examples/cloud-function.ts` is a complete Lambda HTTP API / Function URL
payload v2 example. The adapter converts event headers, query strings, cookies,
and base64 request bodies into a Web `Request`. It returns status, headers,
cookies, and a base64 response body. Payload v1 and ALB events are rejected.
The Lambda adapter buffers responses and defaults to a 4 MiB body limit; set
`maxResponseBytes` only within the provider's full payload limits.

To run that same function **locally**, start a local Axum listener that creates
a Lambda v2 event for each HTTP request and calls the handler:

```sh
node --import @oxc-node/core/register examples/cloud-function-local.ts
node examples/cloud/smoke.mjs http://127.0.0.1:8080
```

Run the smoke command in another terminal. This validates the event adapter
and HTTP behavior locally; it does not emulate Lambda's full lifecycle, IAM,
limits, or deployment package.

For a Cloudflare Worker, export `cloudflare(app)` as the Worker default export.
It passes `env` and `waitUntil` into the app context and supports response
streaming. `examples/cloud/worker.mjs` and `examples/cloud/wrangler.toml`
provide a local Wrangler example. Construct responses inside handler functions
in Workers, because the runtime restricts work at module initialization. Native
N-API imports do not run in Workers. Cloudflare's separate Rust/Wasm path is
illustrated in `examples/cloud-rust/worker`; the same portable Rust application
is used by its local and Lambda examples.

`google(app, { origin })` targets a Functions Framework HTTP handler with
`rawBody`. `azure(app)` targets an Azure Functions v4 HTTP handler. Both buffer
responses with the same default 4 MiB limit. The `origin` for Google must be
configured explicitly. Their platform SDKs and deployment configuration are
not installed by this package. These adapters have contract tests, but have
not been validated in provider emulators or deployed environments.

`requireCapabilities(target, { streaming: true })` can reject a target that
does not declare streaming support. It is a manual assertion, not a scan of
your application's dependency graph. Function runtimes may not expose client
disconnects or support long-lived streams; use a streaming target for SSE.

## Testing and current limits

```sh
npm run build
npm test
npx vp test --run --maxWorkers=1 \
  sources/repository/src/net/server.test.ts \
  sources/repository/src/net/cloud.test.ts
```

The focused command covers routing, response streaming, shutdown, process
liveness, and function-adapter contracts. The workspace-wide `npm test` also
runs unrelated packages. At the latest local run, the build and 16 focused
tests passed; the workspace run stopped at 49 failures in the crypto, KvVault,
and Vault test files. Those failures are not a passing full-suite result.

The portable Rust HTTP types and reusable local Axum adapter are in
`sources/server-core` and `sources/server-local`. They currently use buffered
request and response bodies. For additional target setup and constraints, see
`docs/cloud-targets.md`.
