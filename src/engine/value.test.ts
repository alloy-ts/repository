import { describe, expect, it } from "vite-plus/test";
import { Engine } from "../main.js";

describe("Engine Value Mapping", () => {
  it("handles numbers within safe integer bounds", () => {
    const engine = new Engine();
    const prog = engine.compile("let x = 42; x;");
    expect(prog.run().value).toBe(42);
  });

  it("handles floating point numbers", () => {
    const engine = new Engine();
    const prog = engine.compile("let x = 3.14159; x;");
    expect(prog.run().value).toBeCloseTo(3.14159);
  });

  it("handles booleans and strings", () => {
    const engine = new Engine();
    expect(engine.compile("true;").run().value).toBe(true);
    expect(engine.compile("false;").run().value).toBe(false);
    expect(engine.compile('"hello world";').run().value).toBe("hello world");
  });

  it("handles arrays and nested structures via host function", () => {
    const engine = new Engine();
    engine.registerFunction("getArray", () => [1, 2, [3, 4]]);
    const prog = engine.compile("getArray();");
    expect(prog.run().value).toEqual([1, 2, [3, 4]]);
  });

  it("handles big integers", () => {
    const engine = new Engine();
    engine.registerFunction("getBigInt", () => 9007199254740995n);
    const prog = engine.compile("getBigInt();");
    expect(prog.run().value).toBe(9007199254740995n);
  });

  it("throws unsupported type on plain objects returned from host functions", () => {
    const engine = new Engine();
    engine.registerFunction("getObj", () => ({ a: 1 }));
    const prog = engine.compile("getObj();");
    expect(() => prog.run()).toThrow();
  });
});
