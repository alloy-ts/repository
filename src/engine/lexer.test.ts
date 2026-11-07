import { describe, expect, test } from "vite-plus/test";
import { Engine } from "../../dist/index.mjs";

describe("Engine Lexer tests", () => {
  test("tokenizes identifiers, literals and operators", () => {
    const engine = new Engine();
    const program = engine.compile('let greeting = "hello"; let count = 42;');
    expect(program).toBeDefined();
  });

  test("handles syntax errors on invalid tokens", () => {
    const engine = new Engine();
    expect(() => engine.compile("let @invalid = 1;")).toThrow();
  });
});
