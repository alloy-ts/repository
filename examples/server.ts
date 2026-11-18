/** Local HTTP example: run with `node --import @oxc-node/core/register examples/server.ts`. */
import { Server, Response } from "../sources/repository/src/main.ts";

const port = Number(process.env.PORT ?? 8080);
if (!Number.isInteger(port) || port < 0 || port > 65535) {
  throw new Error("PORT must be an integer from 0 to 65535");
}

const server = new Server({
  host: process.env.HOST ?? "127.0.0.1",
  port,
  routes: {
    "/health": () => Response.json({ ok: true }),
    "/users/:id": (request) => Response.json({ id: request.params["id"] }),
    "/echo": {
      POST: async (request) =>
        new Response(await request.arrayBuffer(), {
          headers: {
            "content-type": request.headers.get("content-type") ?? "application/octet-stream",
          },
        }),
    },
  },
  fetch: () => new Response("Not Found", { status: 404 }),
});

const boundPort = await server.start();
console.log(`Listening at http://${server.host}:${boundPort}`);

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
