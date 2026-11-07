import { afterEach, describe, expect, test } from "vite-plus/test";
import { createServer } from "node:http";
import { createServer as createHttpsServer } from "node:https";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { gzipSync } from "node:zlib";
import { once } from "node:events";
import { Request, Response, Headers, fetch } from "../main.ts";

const cleanup: (() => Promise<void>)[] = [];
afterEach(async () => {
  for (const close of cleanup.splice(0)) await close();
});
async function fixture() {
  const server = createServer(async (req, res) => {
    if (req.url?.startsWith("/redirect")) {
      res.writeHead(Number(req.url.split("/")[2]) || 302, { location: "/echo" });
      res.end();
      return;
    }
    if (req.url === "/binary") {
      res.end(Buffer.from([0, 255, 13, 10, 13, 10, 7]));
      return;
    }
    if (req.url === "/gzip") {
      res.setHeader("content-encoding", "gzip");
      res.end(gzipSync("compressed"));
      return;
    }
    if (req.url === "/close") {
      res.useChunkedEncodingByDefault = false;
      res.setHeader("connection", "close");
      res.end("close-delimited");
      return;
    }
    if (req.url === "/loop") {
      res.writeHead(302, { location: "/loop" });
      res.end();
      return;
    }
    if (req.url === "/empty") {
      res.writeHead(204);
      res.end();
      return;
    }
    const chunks: Buffer[] = [];
    for await (const chunk of req) chunks.push(Buffer.from(chunk));
    res.setHeader("content-type", "application/json");
    res.end(
      JSON.stringify({
        method: req.method,
        body: Buffer.concat(chunks).toString(),
        type: req.headers["content-type"],
      }),
    );
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  cleanup.push(
    () =>
      new Promise<void>((resolve, reject) => {
        server.close((e) => (e ? reject(e) : resolve()));
        server.closeAllConnections();
      }),
  );
  return `http://127.0.0.1:${(server.address() as { port: number }).port}`;
}
describe("Web Request and http_request fetch", () => {
  test("Headers, one-shot bodies, clone and ArrayBuffer types", async () => {
    const request = new Request("http://localhost/", {
      method: "POST",
      headers: new Headers({ "X-Test": "yes" }),
      body: "hello",
    });
    expect(request.headers.get("x-test")).toBe("yes");
    expect(request.body).toBeInstanceOf(ReadableStream);
    const clone = request.clone();
    expect(await request.text()).toBe("hello");
    expect(request.bodyUsed).toBe(true);
    await expect(request.text()).rejects.toThrow();
    expect(() => request.clone()).toThrow();
    expect(await clone.arrayBuffer()).toBeInstanceOf(ArrayBuffer);
    const response = Response.json({ ok: true });
    expect(await response.clone().json()).toEqual({ ok: true });
    expect(await response.json()).toEqual({ ok: true });
    await expect(response.json()).rejects.toThrow();
  });
  test("validates methods, URLs, GET bodies and supports formData", async () => {
    expect(() => new Request("not a URL")).toThrow();
    // Deliberately invalid input exercises the Web constructor's validation.
    // oxlint-disable-next-line unicorn/no-invalid-fetch-options
    expect(() => new Request("http://localhost", { body: "bad" })).toThrow();
    expect(() => new Request("http://localhost", { method: "TRACE" })).toThrow();
    const response = new Response("a=one&b=two", {
      headers: { "content-type": "application/x-www-form-urlencoded" },
    });
    expect((await response.formData()).get("a")).toBe("one");
  });
  test("fetch(Request), start and init overrides preserve actual HTTP bodies", async () => {
    const base = await fixture();
    const request = new Request(`${base}/echo`, { method: "POST", body: "hello" });
    expect(await (await fetch(request)).json()).toMatchObject({ method: "POST", body: "hello" });
    expect(
      await (await new Request(`${base}/echo`, { method: "PUT", body: "start" }).start()).json(),
    ).toMatchObject({ method: "PUT", body: "start" });
    const form = new FormData();
    form.set("name", "value");
    const result = await fetch(`${base}/echo`, { method: "POST", body: form });
    expect(await result.json()).toMatchObject({
      type: expect.stringContaining("multipart/form-data;"),
    });
    expect((await fetch(`${base}/empty`)).body).toBeNull();
    expect(await (await fetch(`${base}/echo`, { method: "HEAD" })).text()).toBe("");
    expect(await (await fetch(`${base}/binary`)).bytes()).toEqual(
      new Uint8Array([0, 255, 13, 10, 13, 10, 7]),
    );
    expect(await (await fetch(`${base}/gzip`)).text()).toBe("compressed");
    expect(await (await fetch(`${base}/close`)).text()).toBe("close-delimited");
  });
  test("relative redirects, method rewriting, 307, manual/error and loops", async () => {
    const base = await fixture();
    for (const status of [301, 302, 303, 307, 308]) {
      const response = await fetch(`${base}/redirect/${status}`, {
        method: "POST",
        body: "payload",
      });
      expect(response.url).toBe(`${base}/echo`);
      expect(response.redirected).toBe(true);
      expect(await response.json()).toMatchObject(
        status < 307 ? { method: "GET", body: "" } : { method: "POST", body: "payload" },
      );
    }
    const manual = await fetch(`${base}/redirect/302`, { redirect: "manual" });
    expect(manual.status).toBe(302);
    expect(manual.redirected).toBe(false);
    await expect(fetch(`${base}/redirect/302`, { redirect: "error" })).rejects.toThrow();
    await expect(fetch(`${base}/loop`)).rejects.toThrow(/redirect/);
    await expect(fetch(`${base}/echo`, { signal: AbortSignal.abort() })).rejects.toThrow();
  });
});

test("HTTPS uses TLS and verifies a locally trusted certificate", async () => {
  // This checked-in key is exclusively a localhost test fixture.
  const certPath = fileURLToPath(new URL("./fixtures/localhost.crt", import.meta.url));
  const server = createHttpsServer(
    {
      cert: readFileSync(certPath),
      key: readFileSync(new URL("./fixtures/localhost.key", import.meta.url)),
    },
    (_req, res) => res.end("secure"),
  );
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const saved = process.env["SSL_CERT_FILE"];
  try {
    await expect(
      fetch(`https://127.0.0.1:${(server.address() as { port: number }).port}`),
    ).rejects.toThrow(/certificate/);
    process.env["SSL_CERT_FILE"] = fileURLToPath(
      new URL("./fixtures/localhost-ca.crt", import.meta.url),
    );
    expect(
      await (
        await fetch(`https://127.0.0.1:${(server.address() as { port: number }).port}`)
      ).text(),
    ).toBe("secure");
  } finally {
    if (saved === undefined) delete process.env["SSL_CERT_FILE"];
    else process.env["SSL_CERT_FILE"] = saved;
    await new Promise<void>((resolve) => {
      server.close(() => resolve());
      server.closeAllConnections();
    });
  }
});
