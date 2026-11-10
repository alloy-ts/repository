import { describe, expect, test } from "vite-plus/test";
import { Engine } from "../../dist/index.mjs";

describe("Engine (FuseVM + NAPI-RS)", () => {
  test("basic arithmetic and operator precedence", () => {
    const engine = new Engine();
    const prog = engine.compile("10 + 20 * 2;");
    const r = prog.run();
    expect(r.value).toBe(50);
    expect(r.stdout).toBe("");
  });

  test("variables, assignment, and print stdout capture", () => {
    const engine = new Engine();
    const prog = engine.compile(`
      let x = 10;
      let y = 20;
      let result = (x + y) * 2;
      print(result);
      result;
    `);
    const r = prog.run();
    expect(r.value).toBe(60);
    expect(r.stdout).toBe("60\n");
  });

  test("while loop (sum 1..100 = 5050)", () => {
    const engine = new Engine({ jit: true });
    const prog = engine.compile(`
      let sum = 0;
      let i = 1;
      while (i <= 100) {
        sum = sum + i;
        i = i + 1;
      }
      print(sum);
      sum;
    `);
    const r = prog.run();
    expect(r.value).toBe(5050);
    expect(r.stdout).toBe("5050\n");
  });

  test("if-else conditionals", () => {
    const engine = new Engine();
    const prog1 = engine.compile(`
      let x = 42;
      let res = 0;
      if (x > 50) {
        res = 1;
      } else {
        res = 2;
      }
      res;
    `);
    expect(prog1.run().value).toBe(2);

    const prog2 = engine.compile(`
      let x = 100;
      let res = 0;
      if (x > 50) {
        res = 1;
      } else {
        res = 2;
      }
      res;
    `);
    expect(prog2.run().value).toBe(1);
  });

  test("user defined functions and recursion (factorial 5 = 120)", () => {
    const engine = new Engine();
    const prog = engine.compile(`
      fn factorial(n) {
        if (n <= 1) {
          return 1;
        }
        return n * factorial(n - 1);
      }
      let result = factorial(5);
      print(result);
      result;
    `);
    const r = prog.run();
    expect(r.value).toBe(120);
    expect(r.stdout).toBe("120\n");
  });

  test("globals parameter in run()", () => {
    const engine = new Engine();
    const prog = engine.compile("x * qty;", ["x", "qty"]);
    const r = prog.run({ x: 5, qty: 3 });
    expect(r.value).toBe(15);
  });

  test("undefined variable errors at compile time", () => {
    const engine = new Engine();
    expect(() => engine.compile("unknown_var * 2;")).toThrow("Undefined variable: unknown_var");
  });

  test("host function callback and stdout capture", () => {
    const engine = new Engine({ jit: true });
    const prices: Record<string, number> = {
      ABC: 50,
      XYZ: 100,
    };
    engine.registerFunction("price", (sym: string) => prices[sym]);

    const prog = engine.compile('print(price("ABC") * 2); price("ABC") * 2;');
    const r = prog.run();
    expect(r.value).toBe(100);
    expect(r.stdout).toBe("100\n");
  });

  test("host function exceptions propagate to caller", () => {
    const engine = new Engine();
    engine.registerFunction("fail", () => {
      throw new Error("Something went wrong in host");
    });
    const prog = engine.compile("fail();");
    expect(() => prog.run()).toThrow("Something went wrong in host");
  });

  test("host function re-entrancy without deadlock", () => {
    const engine = new Engine();
    engine.registerFunction("nested", (n: number) => {
      // Re-entrant call: compiling and running another program inside a host function
      const innerProg = engine.compile("let a = 10; a + 5;");
      const innerRes = innerProg.run();
      return (innerRes.value as number) + n;
    });

    const prog = engine.compile("nested(7);");
    const r = prog.run();
    expect(r.value).toBe(22);
  });

  test("async execution with runAsync()", async () => {
    const engine = new Engine({ jit: true });
    const prog = engine.compile(`
      let sum = 0;
      let i = 1;
      while (i <= 10) {
        sum = sum + i;
        i = i + 1;
      }
      print(sum);
      sum;
    `);
    const r = await prog.runAsync();
    expect(r.value).toBe(55);
    expect(r.stdout).toBe("55\n");
  });

  test("runAsync rejects program using host functions", async () => {
    const engine = new Engine();
    engine.registerFunction("dummy", () => 42);
    const prog = engine.compile("dummy();");
    expect(() => prog.runAsync()).toThrow(
      "Cannot use runAsync on a program that uses host functions",
    );
  });

  test("BigInt handling for numbers exceeding 2^53", () => {
    const engine = new Engine();
    // 2^54 is 18014398509481984
    const prog = engine.compile("9007199254740992 * 2;");
    const r = prog.run();
    expect(typeof r.value).toBe("bigint");
    expect(r.value).toBe(18014398509481984n);
  });

  test("cyclic object and unsupported types handling", () => {
    const engine = new Engine();
    const prog = engine.compile("x;", ["x"]);

    // Cyclic object
    const cycle: Record<string, any> = { a: 1 };
    cycle["self"] = cycle;
    expect(() => prog.run({ x: cycle })).toThrow("Cyclic object references are not supported");

    // Symbol unsupported
    expect(() => prog.run({ x: Symbol("test") })).toThrow("Symbols are not supported as values");
  });
});
