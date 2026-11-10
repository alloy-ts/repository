import { describe, expect, test } from "vite-plus/test";
import { connect, createServer } from "node:net";
import { once } from "node:events";
import { attachHttpSocket, HttpSession, MAX_HTTP_BODY } from "./http-transport.ts";
import { Server } from "./server.ts";

function text(chunks: Array<Buffer | string>): string {
  return Buffer.concat(chunks.map((chunk) => Buffer.from(chunk))).toString();
}

describe("HTTP transport", () => {
  test("parses duplicate headers, chunked bodies, and a bad request line", () => {
    const session = new HttpSession();
    const duplicated = session.push(
      Buffer.from("POST /dup HTTP/1.1\r\nX-Test: one\r\nX-Test: two\r\nContent-Length: 3\r\n\r\nabc"),
    );
    const message = duplicated.find((event) => event.type === "request");
    expect(message && message.type === "request" && message.message.headers).toEqual([
      ["X-Test", "one"],
      ["X-Test", "two"],
      ["Content-Length", "3"],
    ]);
    expect(message && message.type === "request" && message.message.body?.toString()).toBe("abc");

    const chunked = new HttpSession().push(
      Buffer.from("POST /chunk HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nwiki\r\n5\r\npedia\r\n0\r\n\r\n"),
    );
    const body = chunked.find((event) => event.type === "request");
    expect(body && body.type === "request" && body.message.body?.toString()).toBe("wikipedia");

    const malformed = new HttpSession().push(Buffer.from("NOT A REQUEST\r\n\r\n"));
    expect(malformed[0]).toMatchObject({ type: "error", status: 400 });

    const huge = new HttpSession().push(
      Buffer.from(`POST /big HTTP/1.1\r\nContent-Length: ${MAX_HTTP_BODY + 1}\r\n\r\n`),
    );
    expect(huge[0]).toMatchObject({ type: "error", status: 413 });

    const both = new HttpSession().push(
      Buffer.from("POST /both HTTP/1.1\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n"),
    );
    expect(both[0]).toMatchObject({ type: "error", status: 400 });

    const gzip = new HttpSession().push(
      Buffer.from("POST /gzip HTTP/1.1\r\nTransfer-Encoding: gzip\r\n\r\n"),
    );
    expect(gzip[0]).toMatchObject({ type: "error", status: 501 });
  });

  test("answers 100-continue, keep-alive, close, HEAD, and custom methods", async () => {
    const seen: string[] = [];
    const socketServer = createServer((socket) => {
      attachHttpSocket(socket, async (message) => {
        seen.push(`${message.method} ${message.body?.toString() ?? ""}`);
        const body = Buffer.from(message.method === "HEAD" ? "hidden" : "ok");
        return {
          status: 200,
          statusText: "OK",
          headers: [["x-method", message.method]],
          body,
        };
      });
    });
    socketServer.listen(0, "127.0.0.1");
    await once(socketServer, "listening");
    const port = (socketServer.address() as { port: number }).port;
    const client = connect(port, "127.0.0.1");
    const chunks: Buffer[] = [];
    client.on("data", (chunk: Buffer | string) => chunks.push(Buffer.from(chunk)));
    client.write(
      "POST /upload HTTP/1.1\r\nExpect: 100-continue\r\nContent-Length: 4\r\n\r\n",
    );
    await once(client, "data");
    expect(text(chunks)).toContain("100 Continue");
    client.write("data");
    await new Promise((resolve) => setTimeout(resolve, 30));
    client.write("GET /next HTTP/1.1\r\nConnection: close\r\n\r\n");
    await once(client, "end");
    const raw = text(chunks);
    expect(raw).toContain("x-method: POST");
    expect(raw).toContain("x-method: GET");
    expect(seen).toEqual(["POST data", "GET "]);
    client.destroy();

    const head = connect(port, "127.0.0.1");
    const headChunks: Buffer[] = [];
    head.on("data", (chunk: Buffer | string) => headChunks.push(Buffer.from(chunk)));
    head.write("HEAD /item HTTP/1.1\r\nConnection: close\r\n\r\n");
    await once(head, "end");
    const headText = text(headChunks);
    expect(headText).toContain("content-length: 6");
    expect(headText.endsWith("\r\n\r\n")).toBe(true);
    expect(headText).not.toContain("hidden");

    const query = connect(port, "127.0.0.1");
    const queryChunks: Buffer[] = [];
    query.on("data", (chunk: Buffer | string) => queryChunks.push(Buffer.from(chunk)));
    query.write("QUERY /search HTTP/1.1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    await once(query, "end");
    expect(text(queryChunks)).toContain("x-method: QUERY");
    socketServer.close();
  });

  test("the live server decodes chunked bodies and keeps request() in process", async () => {
    const server = new Server({ port: 0 });
    let calls = 0;
    server.post("/chunk", async (c) => {
      calls += 1;
      return c.text(await c.req.raw.text());
    });
    server.get("/local", (c) => {
      calls += 1;
      return c.text(new URL(c.req.url).pathname);
    });
    const port = await server.start();
    try {
      const client = connect(port, "127.0.0.1");
      const chunks: Buffer[] = [];
      client.on("data", (chunk: Buffer | string) => chunks.push(Buffer.from(chunk)));
      client.write(
        "POST /chunk HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
      );
      await once(client, "end");
      expect(text(chunks)).toContain("hello");
      const local = await server.request("http://example.com/local");
      expect(await local.text()).toBe("/local");
      expect(calls).toBe(2);
    } finally {
      await server.stop();
    }
  });
});
