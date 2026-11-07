import { describe, expect, test } from "vite-plus/test";
import { Server, Response } from "../main.ts";

describe("Server (src/net/server.rs)", () => {
  test("Server constructor initializes with routes and default options", () => {
    const server = new Server({
      port: 8080,
      host: "127.0.0.1",
      routes: {
        "/api/status": new Response("OK"),
        "/users/:id": (req: any) => new Response(`Hello User ${req.params.id}!`),
        "/api/*": Response.json({ message: "Not found" }, { status: 404 }),
      },
      fetch(req: any) {
        return new Response("Not Found", { status: 404 });
      },
    });

    expect(server.port).toBe(8080);
    expect(server.host).toBe("127.0.0.1");
    expect(server.listening).toBe(false);

    server.start();
    expect(server.listening).toBe(true);

    server.stop();
    expect(server.listening).toBe(false);
  });

  test("serve() is an alias for start()", () => {
    const server = new Server();
    expect(server.listening).toBe(false);
    server.serve();
    expect(server.listening).toBe(true);
    server.stop();
    expect(server.listening).toBe(false);
  });
});
