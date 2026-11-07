# Network and filesystem bindings

The public package entry is `dist/index.mjs` (source:
`sources/repository/src/main.ts`). `dist/index.js` is the generated, low-level
N-API loader. Use the public entry for Web and Node compatibility; its adapters
are included when `npm run build` packs the package.

| Task requirement | Implementation |
| --- | --- |
| Replace reqwest with http_request | `net/request.rs` executes buffered HTTP(S) through `http_request`; reqwest is absent from manifests, source, and lockfile. A documented local patch fixes defects in 22.2.0. |
| Move protocol into net | `net/protocol.rs` owns the protocol helpers. `repo/protocol.rs` reexports them for repository callers. |
| Request, Response, Headers and fetch | `net/request.ts` uses the runtime's Web implementations for header, body, cloning, form-data and validation semantics. `fetch` and `Request.start()` call the N-API transport, including bounded relative redirects and cross-origin credential stripping. |
| Repository fetch separation | `sources/repo/src/fetch.rs` retains Git remote/ref/object operations. It contained no generic reqwest HTTP operation to extract. HTTP fetch lives in `net/fetch.rs`. |
| Node net/http API | Public `net`/`http` namespaces and `Socket`, `NetServer`, `HttpServer`, `createNetServer`, `connectSocket`, `createHttpServer` use Node's actual streams, events, callbacks and error handling. Native TCP primitives remain in `net/socket.rs`; the fixed-response HTTP placeholder was removed. |
| Rocket server and routing | `net/server.rs` launches Rocket and bridges requests to JS through N-API. `net/server.ts` implements static Response/File routes, named parameters, method handlers (including async handlers), wildcard routes and fallback. `start`/`serve` await socket binding; `stop` awaits shutdown. |
| Node fs API | Public `fs` is the Node fs namespace, including `fs.promises`; existing `fsReadFile` and other N-API primitives remain available. |
| File and Blob | Public Blob is the Web Blob implementation. File is a lazy disk-backed extension with URL/path construction, metadata, exists/exist, write/delete, text/json/bytes/arrayBuffer, incremental ReadableStream and slice. It uses native file operations; File routes and Response(File) stream/read the disk content. |
| Git object names | Git Blob remains available as `GitBlob` and `Object.Blob`. The native class is named GitBlob to avoid a constructor-registry collision with Web Blob. Existing Commit/Tree/Tag exports are retained and also mounted on Object, with generated TypeScript types. |

```ts
import { File, Request, Response, Server, fetch, net, http, fs } from "repository";

const server = new Server({
  port: 0,
  routes: {
    "/api/status": new Response("OK"),
    "/users/:id": req => new Response(`Hello User ${req.params.id}!`),
    "/api/posts": {
      GET: () => new Response("List posts"),
      POST: async req => Response.json({ created: true, ...await req.json() }),
    },
    "/api/*": Response.json({ message: "Not found" }, { status: 404 }),
    "/blog/hello": Response.redirect("/blog/hello/world"),
    "/favicon.ico": new File("./favicon.ico", { type: "image/x-icon" }),
  },
  fetch: () => new Response("Not Found", { status: 404 }),
});
const port = await server.start();
try {
  const request = new Request(`http://127.0.0.1:${port}/api/posts`, {
    method: "POST", body: JSON.stringify({ title: "example" }),
  });
  console.log(await (await request.start()).json());
} finally {
  await server.stop();
}
```

## Draft boundaries

This is a Node.js 24 API backed by native bindings, not a browser implementation
or a claim of complete Web Platform Tests coverage. HTTP response bodies are
buffered before fetch resolves. AbortSignal rejects the caller promptly; an
already running native request finishes or reaches its 30-second socket timeout.
Browser CORS enforcement, cookie storage, HTTP cache, integrity verification,
and streaming upload/download transport are not implemented. The upstream
header map also collapses repeated response headers, including Set-Cookie.

The Rocket bridge buffers each request up to 16 MiB and each response in memory.
It accepts IP literals for host, provides HTTP, and does not implement WebSocket
upgrades or Node-specific server events (use `http` for Node HTTP behavior).

`File(pathOrURL, RequestOpts)` is the requested lazy disk extension, rather than
the browser's `File(parts, name, options)` constructor. The default MIME type is
`text/plain;charset=utf-8`; `type` or a case-insensitive Content-Type header can
override it. Reads report missing files; size is zero for missing files. `slice`
returns a Blob snapshot and currently reads the source synchronously. For Web
APIs that inspect Blob's internal memory rather than calling its methods, pass
`await file.bytes()` or `file.stream()`. The public Response adapter accepts a
File directly. File deletion is synchronous; read and write methods are async.

The localhost TLS key/certificate in `net/fixtures` are public test material,
never production credentials. TLS tests add their CA through SSL_CERT_FILE and
restore the environment afterwards; certificate verification stays enabled.
