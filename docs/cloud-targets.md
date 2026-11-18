# Portable server targets

The native Node server and the reusable local Rust adapter use Axum. The Node
binding is `NativeServer` in Rust and the public TypeScript class remains `Server`.
The native binding compiles ordered route patterns and exposes `addRoute` and
`matchRoute`; the TypeScript wrapper owns handler closures and response streams.
`ServerOptions.strict` defaults to `true`. Supported route syntax includes
static segments, named and optional parameters, wildcards, and inline regex
constraints. The existing `routes` object is registered in insertion order.

The native route matcher is a foundation for a future fluent middleware API;
this change does not expose the proposed `server.use`, context, sub-app and
chained route-builder methods. The Web `createApp` route matcher remains in use
for non-native targets. Do not depend on identical route-priority semantics
between targets until the common route contract is completed.

## Design and implementation plan

The application and the listener have separate responsibilities. Axum remains the
native Node HTTP transport. Cloud functions invoke application handlers through
runtime adapters, without starting Axum or loading N-API on Workers.

1. Extract routing into `net/app.ts`, depending only on Web APIs. Preserve the
   existing Server routes, method dispatch, parameters, files, fallback and errors.
2. Publish a separate `repository/cloud` entry that has no native or Node imports.
3. Add explicit HTTP adapters and capability checks; do not imply support for all
   event types or all features of the underlying platforms.
4. Introduce `alloy-server-core`, a Rust HTTP contract independent of Axum,
   N-API and Tokio. Share transport-header rules with the existing Rust server.
5. Compile the same Rust example application for local Axum, Workers/Wasm and native Lambda.
6. Verify adapter contracts and retain the existing server regression tests.

These components are implemented. Deployment, provisioning and provider account
configuration are deliberately separate from building and testing adapters.

## TypeScript applications

```ts
import { createApp, cloudflare, lambda } from "repository/cloud";

const app = createApp({
  routes: { "/hello/:name": (request) => new Response(`Hello ${request.params.name}`) },
});
export default cloudflare(app); // Worker module entry
export const handler = lambda(app); // Lambda HTTP v2 entry
```

Use separate entry modules for each deployment; runnable examples are in
`examples/cloud`. Cloudflare receives bindings as `context.env`, and its execution
context as `context.platform`. `context.waitUntil` is bound to the Worker context.
Other adapters expose their invocation context through `context.platform`; they
do not invent a portable background-task lifetime.

| Adapter | Supported contract | Response mode |
| --- | --- | --- |
| Existing `Server` / Axum | Local or container HTTP listener | Streaming |
| `cloudflare(app)` | Worker module `fetch` | Streaming |
| `lambda(app)` | API Gateway HTTP API / Function URL payload v2 | Buffered, base64 binary |
| `google(app, { origin })` | Functions Framework HTTP request with `rawBody` | Buffered |
| `azure(app)` | Azure Functions v4 HTTP handler | Buffered |

Register Google with `functions.http("service", google(app, { origin }))` and
Azure with `app.http("service", { methods: [...], handler: azure(application) })`
using the respective platform SDKs. Google requires an explicit canonical origin;
the adapter does not trust arbitrary forwarded host headers. Lambda v1, ALB,
WebSockets, scheduled events and queues are not supported by the TypeScript
Lambda adapter. They must not be silently treated as HTTP v2 events.

Buffered adapters default to a 4 MiB response-body limit; override
`maxResponseBytes` only within your provider's full payload limits (headers and
base64 expansion also count). SSE and never-ending bodies must use streaming
targets. `requireCapabilities(target, { streaming: true })` rejects unsuitable
adapters during application setup; it is an explicit assertion, not an automatic
analysis of application imports. Native dependencies are only available on
compatible native Node runtimes, not Workers.

The shared router strips HEAD bodies, returns 400 for malformed encoded route
parameters, and returns 500 for handler errors. Request cancellation signals pass
through when a Web Request is supplied directly. Function event adapters cannot
reconstruct a client disconnect signal that their invocation interface lacks.

## Rust applications

Cloudflare supports Rust through `workers-rs`, WebAssembly and generated JS glue:
https://developers.cloudflare.com/workers/languages/rust/

Rust support does not imply support for every Rust crate: sockets, filesystem,
native libraries and runtime-specific executors still need an appropriate adapter
or must be excluded from the Wasm dependency graph.

`alloy-server-core` uses `http::Request<Vec<u8>>` and
`http::Response<Vec<u8>>`. This first Rust contract is explicitly buffered.
`examples/cloud-rust/app/src/server.rs` contains the same `/health` and binary
`/echo` application used by local Axum, the Rust Worker and Lambda examples.
Worker bindings (`Env`) and
execution context remain at its entry point; inject needed services into your
application rather than importing worker APIs into the portable core.

The Rust examples do not execute TypeScript route closures or expose the whole
repository addon on Workers. They demonstrate portable Rust application logic.
The SDK-based Rust Lambda entry supports HTTP API, REST API and ALB events through
`lambda_http`; streaming would require a separate response-body contract.
Google/Azure can use the native container path for Rust; their function entry
protocols would need separate Rust adapters if container hosting is unsuitable.

## Build and local development

From the repository root with Node and Rust installed:

```sh
npm run build:native
npx vp test --run --maxWorkers=1 sources/repository/src/net/cloud.test.ts sources/repository/src/net/server.test.ts
node examples/cloud/local.mjs
# In a separate terminal:
curl http://localhost:8080/health
node examples/cloud/smoke.mjs http://localhost:8080

# JavaScript Worker, local runtime (requires Wrangler):
npx wrangler dev --config examples/cloud/wrangler.toml

# Rust checks and Worker build:
cargo test -p alloy-server-core
rustup target add wasm32-unknown-unknown
cargo build --manifest-path examples/cloud-rust/Cargo.toml -p alloy-worker-example --target wasm32-unknown-unknown
cargo check --manifest-path examples/cloud-rust/Cargo.toml -p alloy-lambda-example
PORT=8080 cargo run --manifest-path examples/cloud-rust/Cargo.toml -p alloy-local-example

# Install worker-build separately, then run from examples/cloud-rust/worker:
# cargo install worker-build --locked
# npx wrangler dev

# Native Lambda packaging/local emulation requires cargo-lambda or AWS SAM.
# Compile for the deployment architecture and package the bootstrap executable.
```

Workers must import `repository/cloud`, not the package root, which loads the
native addon. `vp pack` generates the separate entry and its shared router chunk;
include all generated chunks in deployment artifacts. The Rust examples use a
separate Cargo workspace so Worker dependencies cannot contaminate native builds.
Construct responses and streams inside route functions for Workers; its runtime
restricts operations at module initialization that Node permits.

## Limits and remaining work

- Axum's N-API transport still buffers incoming bodies up to 16 MiB. Idle response disconnects
  are detected on a subsequent write; SSE should send heartbeats.
- The Rust Worker caps requests at 4 MiB while collecting chunks. Rust Lambda
  checks the decoded body after the SDK has received it. Both sample applications
  are buffered; neither claims streaming support.
- Native module packaging, secrets, provider quotas and IAM remain deployment
  concerns. No cloud resources are created by this change.
- Adapter tests and compilation are distinct from deployed provider validation.
  Google/Azure handlers require verification in their respective local runtimes
  before production rollout. A future target compiler can automate packaging and
  dependency checks; this version uses explicit entry modules and SDK tooling.
