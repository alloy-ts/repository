import { expect, test } from "vite-plus/test";
import { Server } from "@alloy-ts/net/server";
import { RegExpRouter } from "@alloy-ts/net/server/router/reg-exp-router";

test("@alloy-ts/net/server resolves Server and RegExpRouter", async () => {
  const server = new Server({ router: new RegExpRouter() });
  server.get("/hello", (c) => c.text("hello"));
  expect(await (await server.request("/hello")).text()).toBe("hello");
  server.notFound((c) => c.text("missing", 404));
  server.get("/u/:id", (c) => c.notFound());
  const missing = await server.request("/u/1");
  expect(missing.status).toBe(404);
  expect(await missing.text()).toBe("missing");
});
