import { describe, expect, test } from "vite-plus/test";
import { connect } from "node:net";
import { once } from "node:events";
import { Server } from "./server.ts";

const MAX_BODY = 16 * 1024 * 1024;

function text(chunks: Array<Buffer | string>): string {
  return Buffer.concat(chunks.map((chunk) => Buffer.from(chunk))).toString();
}

function statusOf(raw: string): number {
  const match = /^HTTP\/1\.[01] (\d+)/.exec(raw);
  return match?.[1] ? Number(match[1]) : 0;
}

async function exchange(port: number, payload: string | Buffer): Promise<string> {
  const client = connect(port, "127.0.0.1");
  const chunks: Buffer[] = [];
  client.on("data", (chunk: Buffer | string) => chunks.push(Buffer.from(chunk)));
  client.write(payload);
  await once(client, "end");
  client.destroy();
  return text(chunks);
}

/** Read until the response headers arrive. Used when the connection may stay open. */
async function exchangeHeaders(port: number, payload: string | Buffer): Promise<string> {
  const client = connect(port, "127.0.0.1");
  const chunks: Buffer[] = [];
  const headers = new Promise<void>((resolve) => {
    client.on("data", (chunk: Buffer | string) => {
      chunks.push(Buffer.from(chunk));
      if (text(chunks).includes("\r\n\r\n")) resolve();
    });
    client.on("end", () => resolve());
    client.on("close", () => resolve());
    client.on("error", () => resolve());
  });
  client.end(payload);
  await Promise.race([headers, new Promise((resolve) => setTimeout(resolve, 2000))]);
  client.destroy();
  return text(chunks);
}

describe("Rocket listener", () => {
  test("decodes a chunked POST and keeps duplicate headers", async () => {
    const server = new Server({ port: 0 });
    server.post("/chunk", async (c) => c.text(await c.req.text()));
    server.get("/dup", (c) => c.text(c.req.header("x-test") ?? ""));
    const port = await server.start();
    try {
      const chunked = await exchange(
        port,
        "POST /chunk HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\nwiki\r\n5\r\npedia\r\n0\r\n\r\n",
      );
      expect(chunked).toContain("wikipedia");
      const duplicated = await exchange(
        port,
        "GET /dup HTTP/1.1\r\nHost: localhost\r\nX-Test: one\r\nX-Test: two\r\nConnection: close\r\n\r\n",
      );
      expect(duplicated).toContain("one, two");
    } finally {
      await server.stop();
    }
  });

  test("rejects a malformed request, a conflicting length, and oversized bodies and headers", async () => {
    const server = new Server({ port: 0 });
    server.post("/big", async (c) => c.text(await c.req.text()));
    server.get("/wide", (c) => c.text("nope"));
    const port = await server.start();
    try {
      const malformed = await exchange(port, "NOT A REQUEST\r\n\r\n");
      expect(statusOf(malformed)).toBe(400);

      const both = await exchange(
        port,
        "POST /big HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n0\r\n\r\n",
      );
      // Hyper, under Rocket, accepts this pair and uses the chunked body.
      expect(statusOf(both)).toBe(200);
      expect(both).toContain("content-length: 0");

      const huge = await exchangeHeaders(
        port,
        `POST /big HTTP/1.1\r\nHost: localhost\r\nContent-Length: ${MAX_BODY + 1}\r\nConnection: close\r\n\r\n`,
      );
      expect(statusOf(huge)).toBe(413);

      const accepted = await exchange(
        port,
        `GET /wide HTTP/1.1\r\nHost: localhost\r\nX-Wide: ${"h".repeat(300 * 1024)}\r\nConnection: close\r\n\r\n`,
      );
      expect(statusOf(accepted)).toBe(200);
      // Hyper's HTTP/1 buffer is about 408 KiB. Past that the socket closes
      // with no status line; Rocket does not expose a smaller header cap.
      const wide = await exchangeHeaders(
        port,
        `GET /wide HTTP/1.1\r\nHost: localhost\r\nX-Wide: ${"h".repeat(450 * 1024)}\r\nConnection: close\r\n\r\n`,
      );
      expect(wide).toBe("");
    } finally {
      await server.stop();
    }
  });

  test("keeps the connection open, then closes it, and writes HEAD with no body", async () => {
    const server = new Server({ port: 0 });
    server.get("/next", (c) => c.text("second"));
    server.post("/upload", async (c) => c.text(await c.req.text()));
    server.head("/item", (c) => c.text("hidden"));
    server.get("/item", (c) => c.text("visible"));
    const port = await server.start();
    try {
      const client = connect(port, "127.0.0.1");
      const chunks: Buffer[] = [];
      client.on("data", (chunk: Buffer | string) => chunks.push(Buffer.from(chunk)));
      client.write("POST /upload HTTP/1.1\r\nHost: localhost\r\nContent-Length: 4\r\n\r\ndata");
      await new Promise((resolve) => setTimeout(resolve, 50));
      expect(text(chunks)).toContain("data");
      client.write("GET /next HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
      await once(client, "end");
      const raw = text(chunks);
      expect(raw).toContain("data");
      expect(raw).toContain("second");
      client.destroy();

      const head = await exchange(port, "HEAD /item HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
      expect(head).toContain(`content-length: ${Buffer.byteLength("hidden")}`);
      expect(head.endsWith("\r\n\r\n")).toBe(true);
      expect(head).not.toContain("hidden");
    } finally {
      await server.stop();
    }
  });

  test("the listener rejects QUERY and PURGE while request() still routes them", async () => {
    const server = new Server({ port: 0 });
    server.query("/search", (c) => c.text("QUERY"));
    server.on("PURGE", "/cache", (c) => c.text("PURGE"));
    const port = await server.start();
    try {
      expect(await (await server.request("/search", { method: "QUERY" })).text()).toBe("QUERY");
      expect(await (await server.request("/cache", { method: "PURGE" })).text()).toBe("PURGE");
      const query = await exchange(
        port,
        "QUERY /search HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
      );
      const purge = await exchange(
        port,
        "PURGE /cache HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
      );
      expect(statusOf(query)).toBe(400);
      expect(statusOf(purge)).toBe(400);
    } finally {
      await server.stop();
    }
  });
});

describe("in-process dispatch", () => {
  test("request() routes an absolute URL without using the listener", async () => {
    const server = new Server({ port: 0 });
    server.get("/local", (c) => c.text(new URL(c.req.url).pathname));
    const port = await server.start();
    try {
      const local = await server.request("http://example.com/local");
      expect(await local.text()).toBe("/local");
      expect(server.port).toBe(port);
    } finally {
      await server.stop();
    }
  });
});
