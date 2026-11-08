import { describe, expect, test } from "vite-plus/test";
import { Response, Headers, File } from "../main.ts";
import * as native from "../../../../dist/index.js";

describe("Web Response API conformance", () => {
  test("default constructor creates empty 200 OK response", async () => {
    const res = new Response();
    expect(res.status).toBe(200);
    expect(res.statusText).toBe("");
    expect(res.ok).toBe(true);
    expect(res.redirected).toBe(false);
    expect(res.type).toBe("default");
    expect(res.url).toBe("");
    expect(res.bodyUsed).toBe(false);
    expect(await res.text()).toBe("");
    expect(res.bodyUsed).toBe(false);

    const withBody = new Response("some body");
    expect(withBody.bodyUsed).toBe(false);
    expect(await withBody.text()).toBe("some body");
    expect(withBody.bodyUsed).toBe(true);
  });

  test("status and statusText customization", () => {
    const res = new Response(null, { status: 201, statusText: "Created Successfully" });
    expect(res.status).toBe(201);
    expect(res.statusText).toBe("Created Successfully");
    expect(res.ok).toBe(true);

    const notFound = new Response(null, { status: 404 });
    expect(notFound.status).toBe(404);
    expect(notFound.statusText).toBe("");
    expect(notFound.ok).toBe(false);

    const serverError = new Response(null, { status: 500, statusText: "Internal Server Error" });
    expect(serverError.status).toBe(500);
    expect(serverError.statusText).toBe("Internal Server Error");
    expect(serverError.ok).toBe(false);
  });

  test("headers initialization and inspection", () => {
    const res = new Response("hello", {
      headers: {
        "X-Custom-Header": "custom-value",
        "Content-Type": "text/plain",
      },
    });
    expect(res.headers.get("x-custom-header")).toBe("custom-value");
    expect(res.headers.get("content-type")).toBe("text/plain");

    const withHeadersObj = new Response("test", {
      headers: new Headers({ "X-Foo": "Bar" }),
    });
    expect(withHeadersObj.headers.get("x-foo")).toBe("Bar");
  });

  test("body consuming methods: text, json, arrayBuffer, bytes", async () => {
    const payload = JSON.stringify({ message: "hello", count: 42 });
    const res = new Response(payload, {
      headers: { "content-type": "application/json" },
    });
    expect(await res.json()).toEqual({ message: "hello", count: 42 });
    expect(res.bodyUsed).toBe(true);

    // Body cannot be read twice
    await expect(res.text()).rejects.toThrow();
    await expect(res.arrayBuffer()).rejects.toThrow();
    await expect(res.bytes()).rejects.toThrow();

    const binRes = new Response(Buffer.from([1, 2, 3, 4]));
    const ab = await binRes.arrayBuffer();
    expect(ab).toBeInstanceOf(ArrayBuffer);
    expect(new Uint8Array(ab)).toEqual(new Uint8Array([1, 2, 3, 4]));

    const bytesRes = new Response(new Uint8Array([10, 20, 30]));
    const bytes = await bytesRes.bytes();
    expect(bytes).toBeInstanceOf(Uint8Array);
    expect(bytes).toEqual(new Uint8Array([10, 20, 30]));
  });

  test("formData method parses URL-encoded body", async () => {
    const res = new Response("foo=bar&baz=qux", {
      headers: { "content-type": "application/x-www-form-urlencoded" },
    });
    const fd = await res.formData();
    expect(fd.get("foo")).toBe("bar");
    expect(fd.get("baz")).toBe("qux");
  });

  test("clone() produces an independent copy of response", async () => {
    const res = new Response("original body", {
      status: 202,
      statusText: "Accepted",
      headers: { "X-Test": "123" },
    });
    const clone = res.clone();
    expect(clone.status).toBe(202);
    expect(clone.statusText).toBe("Accepted");
    expect(clone.headers.get("x-test")).toBe("123");

    expect(await res.text()).toBe("original body");
    expect(res.bodyUsed).toBe(true);

    // Cloned response body is still unconsumed
    expect(clone.bodyUsed).toBe(false);
    expect(await clone.text()).toBe("original body");
    expect(clone.bodyUsed).toBe(true);

    // Cannot clone after body has been used
    expect(() => res.clone()).toThrow();
  });

  test("Response.json static factory creates application/json response", async () => {
    const res = Response.json({ key: "value", num: 123 }, { status: 201 });
    expect(res.status).toBe(201);
    expect(res.headers.get("content-type")).toBe("application/json");
    expect(await res.json()).toEqual({ key: "value", num: 123 });
  });

  test("Response.redirect static factory creates 302 redirect response with location", () => {
    const res = Response.redirect("https://example.com/target", 301);
    expect(res.status).toBe(301);
    expect(res.headers.get("location")).toBe("https://example.com/target");

    const defaultRedirect = Response.redirect("https://example.com/default");
    expect(defaultRedirect.status).toBe(302);
    expect(defaultRedirect.headers.get("location")).toBe("https://example.com/default");

    // Invalid redirect status throws
    expect(() => Response.redirect("https://example.com", 200)).toThrow();
    expect(() => Response.redirect("https://example.com", 404)).toThrow();
    expect(() => Response.redirect("https://example.com", 500)).toThrow();
  });

  test("Response.error static factory creates network error response", () => {
    const res = Response.error();
    expect(res.type).toBe("error");
    expect(res.status).toBe(0);
    expect(res.ok).toBe(false);
  });

  test("File body support", async () => {
    const file = new File("./package.json");
    const res = new Response(file);
    const text = await res.text();
    expect(text).toContain('"name": "repository"');
  });

  test("validation of status range and null-body statuses", () => {
    // In WHATWG fetch spec, status must be 200..=599
    expect(() => new Response(null, { status: 199 })).toThrow();
    expect(() => new Response(null, { status: 600 })).toThrow();

    // Null-body statuses cannot have a body
    expect(() => new Response("forbidden body", { status: 204 })).toThrow();
    expect(() => new Response("forbidden body", { status: 304 })).toThrow();
  });
});

describe("Native NAPI Response class", () => {
  test("native constructor and properties", async () => {
    const res = new native.Response("hello world", {
      status: 201,
      statusText: "Created",
      headers: { "x-native": "yes" },
    });
    expect(res.status).toBe(201);
    expect(res.statusText).toBe("Created");
    expect(res.ok).toBe(true);
    expect(res.redirected).toBe(false);
    expect(res.url).toBe("");
    expect(res.type).toBe("default");
    expect(res.headers).toEqual({ "x-native": "yes", "content-type": "text/plain;charset=UTF-8" });
    expect(res.bodyUsed).toBe(false);

    expect(await res.text()).toBe("hello world");
    expect(res.bodyUsed).toBe(true);

    // Cannot read body twice
    await expect(res.text()).rejects.toThrow();
    await expect(res.json()).rejects.toThrow();
    await expect(res.arrayBuffer()).rejects.toThrow();
    await expect(res.bytes()).rejects.toThrow();
    expect(() => res.clone()).toThrow();
  });

  test("native clone() before consuming body", async () => {
    const res = new native.Response("clone-me", { status: 200 });
    const copy = res.clone();
    expect(await res.text()).toBe("clone-me");
    expect(res.bodyUsed).toBe(true);
    expect(copy.bodyUsed).toBe(false);
    expect(await copy.text()).toBe("clone-me");
    expect(copy.bodyUsed).toBe(true);
  });

  test("native Response.json factory", async () => {
    const res = native.Response.json({ alpha: 1, beta: "two" }, { status: 200 });
    expect(res.status).toBe(200);
    expect(res.headers["content-type"]).toBe("application/json");
    expect(await res.json()).toEqual({ alpha: 1, beta: "two" });
  });

  test("native Response.redirect factory and validation", () => {
    const res = native.Response.redirect("http://example.com/dest", 307);
    expect(res.status).toBe(307);
    expect(res.headers["location"]).toBe("http://example.com/dest");

    expect(() => native.Response.redirect("http://example.com", 200)).toThrow();
    expect(() => native.Response.redirect("not a valid url", 302)).toThrow();
  });

  test("native Response.error factory", () => {
    const res = native.Response.error();
    expect(res.status).toBe(0);
    expect(res.type).toBe("error");
    expect(res.ok).toBe(false);
  });

  test("native status validation and null-body rejection", () => {
    expect(() => new native.Response(null, { status: 100 })).toThrow();
    expect(() => new native.Response(null, { status: 601 })).toThrow();
    expect(() => new native.Response("body", { status: 204 })).toThrow();
    expect(() => new native.Response("body", { status: 304 })).toThrow();
    expect(new native.Response(null, { status: 204 }).status).toBe(204);
  });
});
