import { describe, expect, test } from "vite-plus/test";
import { Engine } from "../../dist/index.mjs";

describe("Engine Compiler tests", () => {
  test("compiles expressions and assignments", () => {
    const engine = new Engine();
    const program = engine.compile("let a = 10; let b = 20; a + b;");
    const result = program.run();
    expect(result.value).toBe(30);
  });

  test("compiles function declarations and calls", () => {
    const engine = new Engine();
    const program = engine.compile(`
      fn add(x, y) {
        return x + y;
      }
      add(5, 7);
    `);
    const result = program.run();
    expect(result.value).toBe(12);
  });

  test("compiles while loops and conditionals", () => {
    const engine = new Engine();
    const program = engine.compile(`
      let sum = 0;
      let i = 1;
      while (i <= 5) {
        sum = sum + i;
        i = i + 1;
      }
      sum;
    `);
    const result = program.run();
    expect(result.value).toBe(15);
  });

  test("compiles host function calls", () => {
    const engine = new Engine();
    engine.registerFunction("mul", (a: number, b: number) => a * b);
    const program = engine.compile("mul(6, 7);");
    const result = program.run();
    expect(result.value).toBe(42);
  });
});
