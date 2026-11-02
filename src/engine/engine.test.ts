import { describe, expect, test } from "vite-plus/test";
import { Engine } from "../main.js";

describe("Engine NAPI bindings with FuseVM", () => {
  test("executes arithmetic expressions and prints output", () => {
    const engine = new Engine();
    const result = engine.execute("let x = 10; let y = 20; let result = (x + y) * 2; print(result);");
    expect(result.stdout.trim()).toBe("60");
  });

  test("executes while loops correctly", () => {
    const engine = new Engine();
    const code = `
      let sum = 0;
      let i = 1;
      while (i <= 100) {
        sum = sum + i;
        i = i + 1;
      }
      print(sum);
    `;
    const prog = engine.compile(code);
    const res = prog.run();
    expect(res.stdout.trim()).toBe("5050");
  });

  test("supports function recursion", () => {
    const engine = new Engine({ jit: true });
    const code = `
      fn factorial(n) {
        if (n <= 1) {
          return 1;
        }
        return n * factorial(n - 1);
      }
      let result = factorial(5);
      print(result);
    `;
    const prog = engine.compile(code);
    const res = prog.run();
    expect(res.stdout.trim()).toBe("120");
  });

  test("handles if / else conditions", () => {
    const engine = new Engine();
    const code = `
      let x = 42;
      if (x > 50) {
        print("greater");
      } else {
        print("lesser or equal");
      }
    `;
    const res = engine.execute(code);
    expect(res.stdout.trim()).toBe("lesser or equal");
  });

  test("throws compile error on invalid syntax", () => {
    const engine = new Engine();
    expect(() => {
      engine.compile("let = 123;");
    }).toThrow();
  });
});
