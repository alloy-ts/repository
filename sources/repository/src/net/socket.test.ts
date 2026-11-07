import { expect, test } from "vite-plus/test";
import { once } from "node:events";
import { createNetServer, connectSocket, createHttpServer, http, fs } from "../main.ts";
import * as native from "../../../../dist/index.js";

test("Node TCP streams exchange data and expose connection metadata", async () => {
  const server = createNetServer((socket) => socket.pipe(socket));
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const client = connectSocket((server.address() as { port: number }).port, "127.0.0.1");
  try {
    await once(client, "connect");
    expect(client.remoteAddress).toBe("127.0.0.1");
    const data = once(client, "data");
    expect(typeof client.write("echo")).toBe("boolean");
    expect((await data)[0].toString()).toBe("echo");
  } finally {
    client.destroy();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
});
test("Node HTTP invokes listeners and writes the requested status, headers and body", async () => {
  const server = createHttpServer(async (req, res) => {
    let body = "";
    for await (const chunk of req) body += chunk;
    res.writeHead(202, { "x-test": "handler" });
    res.end(`${req.method}:${body}`);
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  try {
    const response = await globalThis.fetch(
      `http://127.0.0.1:${(server.address() as { port: number }).port}`,
      { method: "POST", body: "body" },
    );
    expect(response.status).toBe(202);
    expect(response.headers.get("x-test")).toBe("handler");
    expect(await response.text()).toBe("POST:body");
    expect(http.createServer).toBe(createHttpServer);
    expect(typeof fs.promises.readFile).toBe("function");
  } finally {
    await new Promise<void>((resolve) => {
      server.close(() => resolve());
      server.closeAllConnections();
    });
  }
});
test("native TCP close cancels pending accept and releases its listening port", async () => {
  const server = new native.NetServer();
  const port = await server.listen(0, "127.0.0.1");
  const pending = server.accept();
  const rejected = expect(pending).rejects.toThrow(/closed/);
  await server.close();
  const next = new native.NetServer();
  await next.listen(port, "127.0.0.1");
  await next.close();
  await rejected;
});
test("native destroy cancels an outstanding read", async () => {
  const server = new native.NetServer();
  const port = await server.listen(0, "127.0.0.1");
  const accepted = server.accept();
  const client = await native.connectSocket(port, "127.0.0.1");
  const peer = await accepted;
  try {
    const read = client.read();
    await client.destroy();
    expect(await read).toBeNull();
    expect(await peer.read()).toBeNull();
    await expect(client.write("late")).rejects.toThrow();
  } finally {
    await client.destroy();
    await peer.destroy();
    await server.close();
  }
});
