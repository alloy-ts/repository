/** A portable request handler and its AWS HTTP API / Function URL v2 entry. */
import { createApp, lambda } from "../sources/repository/src/net/cloud.ts";

export const app = createApp({
  routes: {
    "/health": () => Response.json({ ok: true }),
    "/users/:id": (request) => Response.json({ id: request.params["id"] }),
    "/echo": { POST: async (request) => new Response(await request.arrayBuffer()) },
  },
});

export const handler = lambda(app);
