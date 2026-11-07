import { describe, expect, test } from "vite-plus/test";
import { Server, Response } from "../main.ts";

describe("Server (src/net/server.rs)", () => {
  test("Server constructor initializes with routes and default options", async () => {
    const server = new Server({
      port: 8085,
      host: "127.0.0.1",
      routes: {
        "/api/status": new Response("OK"),
        "/api/*": Response.json({ message: "Not found" }, { status: 404 }),
      },
    });

    expect(server.port).toBe(8085);
    expect(server.host).toBe("127.0.0.1");
    expect(server.listening).toBe(false);

    const port = await server.start();
    expect(server.listening).toBe(true);
    expect(port).toBe(8085);

    await server.stop();
    expect(server.listening).toBe(false);
  });

  test("serve() is an alias for start()", async () => {
    const server = new Server({ port: 8086 });
    expect(server.listening).toBe(false);
    await server.serve();
    expect(server.listening).toBe(true);
    await server.stop();
    expect(server.listening).toBe(false);
  });
});
