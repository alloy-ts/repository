import { describe, expect, test } from "vite-plus/test";
import {
  createApp,
  cloudflare,
  lambda,
  google,
  azure,
  requireCapabilities,
  type LambdaHttpEvent,
} from "./cloud.ts";

const event = (overrides: Partial<LambdaHttpEvent> = {}): LambdaHttpEvent => ({
  version: "2.0",
  rawPath: "/echo",
  rawQueryString: "x=1&x=2",
  headers: {},
  requestContext: { domainName: "example.test", http: { method: "POST" } },
  ...overrides,
});
describe("portable application", () => {
  test("routing, precedence, method dispatch, static reuse and malformed parameters", async () => {
    const app = createApp({
      routes: {
        "/u/*": new Response("wild"),
        "/u/:id": { GET: (r) => new Response(r.params["id"]) },
        "/u/fixed": new Response("fixed"),
      },
    });
    for (let i = 0; i < 2; i++)
      expect(await (await app(new Request("https://test/u/fixed"))).text()).toBe("fixed");
    expect(await (await app(new Request("https://test/u/a%20b"))).text()).toBe("a b");
    expect((await app(new Request("https://test/u/%FF"))).status).toBe(400);
    expect((await app(new Request("https://test/missing"))).status).toBe(404);
    expect(await (await app(new Request("https://test/u/fixed", { method: "HEAD" }))).text()).toBe(
      "",
    );
  });
  test("exceptions become 500 responses", async () => {
    expect(
      (
        await createApp({
          fetch: () => {
            throw new Error("secret");
          },
        })(new Request("https://test"))
      ).status,
    ).toBe(500);
  });
  test("worker forwards environment, waitUntil binding, and unbuffered streams", async () => {
    let controller!: ReadableStreamDefaultController<Uint8Array>;
    const tasks: Promise<unknown>[] = [];
    const context = {
      tasks,
      waitUntil(task: Promise<unknown>) {
        this.tasks.push(task);
      },
    };
    const worker = cloudflare(
      createApp({
        fetch: (_request, ctx) => {
          expect(ctx.env).toEqual({ KEY: "value" });
          ctx.waitUntil!(Promise.resolve());
          return new Response(
            new ReadableStream({
              start(c) {
                controller = c;
                c.enqueue(new Uint8Array([1]));
              },
            }),
          );
        },
      }),
    );
    const response = await worker.fetch(new Request("https://test"), { KEY: "value" }, context);
    expect((await response.body!.getReader().read()).value).toEqual(new Uint8Array([1]));
    controller.close();
    expect(tasks).toHaveLength(1);
  });
});
describe("function adapters", () => {
  test("Lambda preserves binary data, repeated query parameters and cookies", async () => {
    const handler = lambda(
      createApp({
        fetch: async (request) => {
          expect(new URL(request.url).searchParams.getAll("x")).toEqual(["1", "2"]);
          expect(request.headers.get("cookie")).toBe("a=1; b=2");
          const headers = new Headers();
          headers.append("set-cookie", "a=1; Path=/");
          headers.append("set-cookie", "b=2; Path=/");
          return new Response(await request.arrayBuffer(), { status: 201, headers });
        },
      }),
    );
    const result = await handler(
      event({ body: "AP8B", isBase64Encoded: true, cookies: ["a=1", "b=2"] }),
    );
    expect(result).toMatchObject({
      statusCode: 201,
      body: "AP8B",
      isBase64Encoded: true,
      cookies: ["a=1; Path=/", "b=2; Path=/"],
    });
    await expect(handler(event({ version: "1.0" }))).rejects.toThrow("2.0");
  });
  test("buffered targets reject oversized responses and cancel the producer", async () => {
    let cancelled = false;
    const handler = lambda(
      createApp({
        fetch: () =>
          new Response(
            new ReadableStream({
              start(c) {
                c.enqueue(new Uint8Array(5));
              },
              cancel() {
                cancelled = true;
              },
            }),
          ),
      }),
      { maxResponseBytes: 4 },
    );
    await expect(handler(event())).rejects.toThrow("maxResponseBytes");
    expect(cancelled).toBe(true);
    expect(() => requireCapabilities("lambda", { streaming: true })).toThrow();
    expect(() => requireCapabilities("cloudflare", { native: true })).toThrow();
  });
  test("Google HTTP and Azure v4 preserve status, headers and binary bodies", async () => {
    const app = createApp({
      fetch: async (request) =>
        new Response(await request.arrayBuffer(), { status: 202, headers: { "x-test": "yes" } }),
    });
    let status = 0;
    const headers = new Map<string, string | string[]>();
    let body = new Uint8Array();
    await google(app, { origin: "https://test" })(
      { method: "POST", originalUrl: "/echo", headers: {}, rawBody: new Uint8Array([0, 255]) },
      {
        status(code) {
          status = code;
        },
        setHeader(name, value) {
          headers.set(name, value);
        },
        end(value) {
          body = new Uint8Array(value);
        },
      },
    );
    expect(status).toBe(202);
    expect(headers.get("x-test")).toBe("yes");
    expect(body).toEqual(new Uint8Array([0, 255]));
    const result = await azure(app)(
      new Request("https://test/echo", { method: "POST", body: new Uint8Array([0, 255]) }),
    );
    expect(result.status).toBe(202);
    expect(result.headers).toContainEqual(["x-test", "yes"]);
    expect(result.body).toEqual(new Uint8Array([0, 255]));
  });
});
