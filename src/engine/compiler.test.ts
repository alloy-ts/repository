import { describe, expect, it } from "vite-plus/test";
import { Engine } from "../main.js";

describe("Engine Compiler & Execution", () => {
  it("compiles and runs arithmetic with proper operator precedence", () => {
    const engine = new Engine();
    const prog = engine.compile("2 + 3 * 4 - 6 / 2;");
    // 2 + 12 - 3 = 11
    expect(prog.run().value).toBe(11);
  });

  it("handles complex loops and variable assignments", () => {
    const engine = new Engine();
    const prog = engine.compile(`
      let a = 0;
      let b = 1;
      let count = 0;
      while (count < 10) {
        let temp = a + b;
        a = b;
        b = temp;
        count = count + 1;
      }
      a;
    `);
    // 10th Fibonacci number starting 0, 1 -> 55
    expect(prog.run().value).toBe(55);
  });

  it("handles multiple sequential function calls", () => {
    const engine = new Engine();
    const prog = engine.compile(`
      fn square(x) {
        return x * x;
      }
      square(3) + square(4);
    `);
    expect(prog.run().value).toBe(25);
  });

  it("detects duplicate variable declarations in the same scope", () => {
    const engine = new Engine();
    expect(() => {
      engine.compile("let x = 1; let x = 2;");
    }).toThrow();
  });

  it("detects undefined variable references at compile time", () => {
    const engine = new Engine();
    expect(() => {
      engine.compile("let y = x + 1;");
    }).toThrow();
  });

  it("detects wrong function arity at compile time", () => {
    const engine = new Engine();
    expect(() => {
      engine.compile("fn foo(a, b) { return a + b; } foo(1);");
    }).toThrow();
  });
});
