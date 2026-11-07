import { describe, expect, test } from "vite-plus/test";
import { Engine } from "../../dist/index.mjs";

describe("Engine Parser tests", () => {
  test("parses basic arithmetic and precedence", () => {
    const engine = new Engine();
    const program = engine.compile("let x = 10 + 20 * 2; x;");
    const result = program.run();
    expect(result.value).toBe(50);
  });

  test("parses functions and recursive control flow", () => {
    const engine = new Engine();
    const program = engine.compile(`
      fn fact(n) {
        if (n <= 1) {
          return 1;
        } else {
          return n * fact(n - 1);
        }
      }
      fact(5);
    `);
    const result = program.run();
    expect(result.value).toBe(120);
  });

  test("throws on parser syntax errors", () => {
    const engine = new Engine();
    expect(() => engine.compile("let = 10;")).toThrow();
  });
});
