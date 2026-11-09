import { describe, expect, test } from "vite-plus/test";
import { RegExpRouter, SmartRouter, TrieRouter, UnsupportedPathError } from "./router.ts";

describe("routers", () => {
  test("RegExpRouter matches every registered handler and rejects conflicting parameters", () => {
    const router = new RegExpRouter<(path: string) => string>();
    router.add("GET", "/book/a", () => "a");
    router.add("GET", "/book/:slug", () => "common");
    router.add("ALL", "*", () => "any");
    const book = router.match("GET", "/book/a").map((route) => route.handler(""));
    expect(book).toEqual(["a", "common", "any"]);
    expect(router.match("GET", "/book/a")[1]?.params).toEqual({ slug: "a" });
    expect(router.match("POST", "/book/a").map((route) => route.handler(""))).toEqual(["any"]);

    router.add("GET", "/post/:id{[0-9]+}", () => "num");
    expect(() => router.add("GET", "/post/:name{[a-z]+}", () => "word")).toThrow(UnsupportedPathError);
    expect(() => new RegExpRouter().add("GET", "/:id{.*}", () => "empty")).toThrow(
      UnsupportedPathError,
    );
  });

  test("TrieRouter keeps conflicting patterns in registration order", () => {
    const router = new TrieRouter<string>();
    router.add("GET", "/post/:id{[0-9]+}", "num");
    router.add("GET", "/post/:name{[a-z]+}", "word");
    router.add("GET", "/post/:id{.+}", "any");
    expect(router.match("GET", "/post/12").map((route) => route.handler)).toEqual(["num", "any"]);
    expect(router.match("GET", "/post/12")[0]?.params).toEqual({ id: "12" });
    expect(router.match("GET", "/post/ab").map((route) => route.handler)).toEqual(["word", "any"]);
    expect(router.match("GET", "/files/images/cat.png")).toEqual([]);
    router.add("GET", "/files/:filename{.+\\.png}", "file");
    expect(router.match("GET", "/files/images/cat.png")[0]?.params["filename"]).toBe(
      "images/cat.png",
    );
  });

  test("SmartRouter stays on RegExpRouter until a pattern cannot be combined", () => {
    const router = new SmartRouter<string>();
    router.add("GET", "/hello", "hello");
    expect(router.active).toBe("RegExpRouter");
    expect(router.match("GET", "/hello")[0]?.handler).toBe("hello");
    router.add("GET", "/post/:id{[0-9]+}", "num");
    router.add("GET", "/post/:name{[a-z]+}", "word");
    expect(router.active).toBe("TrieRouter");
    expect(router.match("GET", "/hello")[0]?.handler).toBe("hello");
    expect(router.match("GET", "/post/4").map((route) => route.handler)).toEqual(["num"]);
    expect(router.match("GET", "/post/ab").map((route) => route.handler)).toEqual(["word"]);
  });
});
