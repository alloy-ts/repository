import { createApp } from "../../dist/cloud.mjs";

export const app = createApp({
  routes: {
    "/health": () => Response.json({ ok: true }),
    "/echo": { POST: async (request) => new Response(await request.arrayBuffer()) },
  },
});
