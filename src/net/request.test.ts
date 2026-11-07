import { describe, expect, it } from "vite-plus/test";
import { Request, Response } from "../main.ts";

describe("Request & Response (src/net/request.rs)", () => {
  it("constructs a Request conforming to Mozilla Web API", async () => {
    const req = new Request("https://example.com/api/test", {
      method: "POST",
      body: "request payload",
      headers: {
        "content-type": "text/plain",
        authorization: "Bearer token123",
      },
    });

    expect(req.url).toBe("https://example.com/api/test");
    expect(req.method).toBe("POST");
    expect(req.headers["content-type"]).toBe("text/plain");
    expect(req.headers["authorization"]).toBe("Bearer token123");
    expect(req.bodyUsed).toBe(false);

    expect(req.text()).toBe("request payload");
    expect(req.bodyUsed).toBe(true);

    const cloned = req.clone();
    expect(cloned.url).toBe(req.url);
    expect(cloned.method).toBe("POST");
  });

  it("constructs a Request from an object with url property", () => {
    const req = new Request({
      url: "https://example.com/get-json",
      method: "GET",
      headers: { accept: "application/json" },
    });

    expect(req.url).toBe("https://example.com/get-json");
    expect(req.method).toBe("GET");
    expect(req.headers["accept"]).toBe("application/json");
  });

  it("supports Response constructor and instance methods", async () => {
    const res = new Response("response text", {
      status: 201,
      statusText: "Created",
      headers: { "x-custom": "header-value" },
    });

    expect(res.status).toBe(201);
    expect(res.statusText).toBe("Created");
    expect(res.ok).toBe(true);
    expect(res.headers["x-custom"]).toBe("header-value");
    expect(await res.text()).toBe("response text");
    expect(await res.arrayBuffer()).toEqual(Buffer.from("response text"));
  });

  it("supports Response.json and Response.redirect static factories", async () => {
    const jsonRes = Response.json({ success: true, count: 5 });
    expect(jsonRes.status).toBe(200);
    expect(jsonRes.ok).toBe(true);
    expect(jsonRes.headers["content-type"]).toBe("application/json");
    expect(await jsonRes.json()).toEqual({ success: true, count: 5 });

    const redir = Response.redirect("https://example.com/next", 301);
    expect(redir.status).toBe(301);
    expect(redir.headers["location"]).toBe("https://example.com/next");
  });
});
