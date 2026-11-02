import { describe, expect, test } from "vite-plus/test";
import { Engine } from "./main.ts";

describe("Engine with FuseVM NAPI bindings", () => {
  test("compiles and runs basic arithmetic and print statements", () => {
    const engine = new Engine({ jit: true });
    const prog = engine.compile(`
      let x = 10;
      let y = 20;
      let result = (x + y) * 2;
      print(result);
    `);
    const r = prog.run();
    expect(r.stdout.trim()).toBe("60");
  });

  test("compiles expression statements returning values", () => {
    const engine = new Engine();
    const prog = engine.compile("1 + 2;");
    const r = prog.run();
    expect(r.value).toBe(3);
  });

  test("supports preloaded globals via run({ qty: 3 })", () => {
    const engine = new Engine();
    const prog = engine.compile(
      `
      let total = qty * 5;
      print(total);
      total;
    `,
      ["qty"],
    );
    const r = prog.run({ qty: 3 });
    expect(r.stdout.trim()).toBe("15");
    expect(r.value).toBe(15);
  });

  test("compiles and runs while loop", () => {
    const engine = new Engine();
    const prog = engine.compile(`
      let sum = 0;
      let i = 1;
      while (i <= 100) {
        sum = sum + i;
        i = i + 1;
      }
      print(sum);
    `);
    const r = prog.run();
    expect(r.stdout.trim()).toBe("5050");
  });

  test("compiles and runs if-else statements", () => {
    const engine = new Engine();
    const prog = engine.compile(`
      let x = 42;
      if (x > 50) {
        print("greater");
      } else {
        print("smaller");
      }
    `);
    const r = prog.run();
    expect(r.stdout.trim()).toBe("smaller");
  });

  test("runs program asynchronously via runAsync()", async () => {
    const engine = new Engine({ jit: false });
    const prog = engine.compile(`
      let a = 15;
      let b = 25;
      print(a + b);
    `);
    const r = await prog.runAsync();
    expect(r.stdout.trim()).toBe("40");
  });

  test("registers and calls host functions returning values into FuseVM", () => {
    const engine = new Engine();
    const prices: Record<string, number> = {
      ABC: 15,
      XYZ: 40,
    };

    engine.registerFunction("price", (sym: string) => {
      return prices[sym] ?? 0;
    });

    const prog = engine.compile(`
      let val = price("ABC") * 2;
      print(val);
      val;
    `);
    const r = prog.run();
    expect(r.stdout.trim()).toBe("30");
    expect(r.value).toBe(30);
  });

  test("runAsync rejects programs with registered host functions", async () => {
    const engine = new Engine();
    engine.registerFunction("foo", () => 42);
    const prog = engine.compile("foo();");
    await expect(prog.runAsync()).rejects.toThrow(/host functions/);
  });
});
