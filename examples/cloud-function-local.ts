/** Run the Lambda HTTP v2 handler over a local Axum listener. */
import { Server } from "../sources/repository/src/main.ts";
import { handler } from "./cloud-function.ts";

const port = Number(process.env.PORT ?? 8080);
if (!Number.isInteger(port) || port < 0 || port > 65535) {
  throw new Error("PORT must be an integer from 0 to 65535");
}

const server = new Server({
  host: "127.0.0.1",
  port,
  fetch: async (request) => {
    const url = new URL(request.url);
    const body = ["GET", "HEAD"].includes(request.method)
      ? undefined
      : Buffer.from(await request.arrayBuffer()).toString("base64");
    const result = await handler({
      version: "2.0",
      rawPath: url.pathname,
      rawQueryString: url.search.slice(1),
      headers: Object.fromEntries(request.headers),
      body,
      isBase64Encoded: body !== undefined,
      requestContext: { domainName: "localhost", http: { method: request.method } },
    });
    const headers = new Headers(result.headers);
    for (const cookie of result.cookies) headers.append("set-cookie", cookie);
    return new Response(Buffer.from(result.body, "base64"), {
      status: result.statusCode,
      headers,
    });
  },
});

const boundPort = await server.start();
console.log(`Lambda HTTP v2 example at http://127.0.0.1:${boundPort}`);
let stopping = false;
async function stop() {
  if (stopping) return;
  stopping = true;
  await server.stop();
}
process.once("SIGINT", () => {
  void stop();
});
process.once("SIGTERM", () => {
  void stop();
});
