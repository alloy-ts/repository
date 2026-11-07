import { describe, expect, test } from "vite-plus/test";
import { Request, Response } from "../main.ts";

describe("Request & Response (src/net/request.rs)", () => {
  test("Request constructor sets url, method and parses headers", async () => {
    const req = new Request("http://localhost:3000/api", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ hello: "world" }),
    });

    expect(req.url).toBe("http://localhost:3000/api");
    expect(req.method).toBe("POST");
    expect(req.headers["content-type"]).toBe("application/json");
    expect(req.bodyUsed).toBe(false);

    const json = await req.json();
    expect(json).toEqual({ hello: "world" });
    expect(req.bodyUsed).toBe(true);
  });

  test("Response constructor and static methods work properly", async () => {
    const res = new Response("OK", { status: 200 });
    expect(res.status).toBe(200);
    expect(res.ok).toBe(true);
    expect(await res.text()).toBe("OK");

    const jsonRes = Response.json({ success: true }, { status: 201 });
    expect(jsonRes.status).toBe(201);
    expect(await jsonRes.json()).toEqual({ success: true });

    const redirectRes = Response.redirect("http://localhost:3000/login", 302);
    expect(redirectRes.status).toBe(302);
    expect(redirectRes.headers["location"]).toBe("http://localhost:3000/login");
  });

  test("Request clone preserves properties", async () => {
    const req = new Request("http://localhost:3000/test", {
      method: "GET",
    });
    const cloned = req.clone();
    expect(cloned.url).toBe(req.url);
    expect(cloned.method).toBe(req.method);
  });
});
