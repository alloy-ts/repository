import { expect, test } from "vite-plus/test";
import { Server } from "../main.ts";

test("set and get share request variables", async () => {
  const server = new Server();
  server.use("/auth/*", async (c, next) => {
    c.set("user", c.env?.TOKEN);
    await next();
  });
  server.get("/auth/me", (c) => c.text(String(c.get("user"))));
  expect(await (await server.fetch(new Request("http://localhost/auth/me"), { TOKEN: "s" })).text()).toBe("s");
  expect(server).toBeInstanceOf(Server);
});
