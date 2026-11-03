import { describe, expect, it } from "vite-plus/test";
import { Engine } from "../main.js";

describe("Engine - Basics & Arithmetic", () => {
  it("evaluates simple integer arithmetic", () => {
    const engine = new Engine();
    const prog = engine.compile("10 + 20 * 2;");
    const r = prog.run();
    expect(r.value).toBe(50);
    expect(r.stdout).toBe("");
  });

  it("evaluates float division per FuseVM Div", () => {
    const engine = new Engine();
    const prog = engine.compile("7 / 2;");
    const r = prog.run();
    expect(r.value).toBe(3.5);
  });

  it("handles negative numbers and unary operators", () => {
    const engine = new Engine();
    const prog = engine.compile("-5 + 10;");
    expect(prog.run().value).toBe(5);

    const progNot = engine.compile("!false;");
    expect(progNot.run().value).toBe(true);
  });

  it("handles comparison operators", () => {
    const engine = new Engine();
    expect(engine.compile("10 > 5;").run().value).toBe(true);
    expect(engine.compile("10 < 5;").run().value).toBe(false);
    expect(engine.compile("10 >= 10;").run().value).toBe(true);
    expect(engine.compile("5 <= 10;").run().value).toBe(true);
    expect(engine.compile("5 == 5;").run().value).toBe(true);
    expect(engine.compile("5 != 5;").run().value).toBe(false);
  });
});

describe("Engine - Variables & Control Flow", () => {
  it("handles variables and while loops", () => {
    const engine = new Engine();
    const source = `
      let sum = 0;
      let i = 1;
      while (i <= 100) {
        sum = sum + i;
        i = i + 1;
      }
      sum;
    `;
    const prog = engine.compile(source);
    expect(prog.run().value).toBe(5050);
  });

  it("handles if-else statements", () => {
    const engine = new Engine();
    const source = `
      let x = 10;
      let res = 0;
      if (x > 5) {
        res = 1;
      } else {
        res = 2;
      }
      res;
    `;
    const prog = engine.compile(source);
    expect(prog.run().value).toBe(1);
  });

  it("handles scoping and shadowing", () => {
    const engine = new Engine();
    const source = `
      let a = 1;
      {
        let a = 2;
        let b = a + 1;
      }
      a;
    `;
    const prog = engine.compile(source);
    expect(prog.run().value).toBe(1);
  });
});

describe("Engine - Functions", () => {
  it("compiles and executes user functions with recursion", () => {
    const engine = new Engine();
    const source = `
      fn factorial(n) {
        if (n <= 1) {
          return 1;
        }
        return n * factorial(n - 1);
      }
      factorial(5);
    `;
    const prog = engine.compile(source);
    expect(prog.run().value).toBe(120);
  });

  it("supports multiple functions and parameter passing", () => {
    const engine = new Engine();
    const source = `
      fn add(a, b) {
        return a + b;
      }
      fn mul(a, b) {
        return a * b;
      }
      add(mul(2, 3), 4);
    `;
    const prog = engine.compile(source);
    expect(prog.run().value).toBe(10);
  });
});

describe("Engine - Globals & Output", () => {
  it("preloads globals passed to run()", () => {
    const engine = new Engine();
    const prog = engine.compile("let x = 10; x * 2;");
    const r = prog.run({ x: 25 });
    expect(r.value).toBe(50);
  });

  it("captures print output into stdout", () => {
    const engine = new Engine();
    const prog = engine.compile('print("hello"); print(42);');
    const r = prog.run();
    expect(r.value).toBeUndefined();
    expect(r.stdout).toBe("hello\n42\n");
  });

  it("throws ENGINE_UNKNOWN_GLOBAL for unrecognized globals", () => {
    const engine = new Engine();
    const prog = engine.compile("let x = 10; x * 2;");
    expect(() => {
      prog.run({ unknownVar: 123 });
    }).toThrow();

    try {
      prog.run({ unknownVar: 123 });
    } catch (e: any) {
      expect(e.code).toBe("ENGINE_UNKNOWN_GLOBAL");
    }
  });
});

describe("Engine - Host Functions", () => {
  it("allows registering and calling host functions", () => {
    const engine = new Engine();
    const prices: Record<string, number> = { ABC: 42, XYZ: 10 };
    engine.registerFunction("price", (sym: any) => prices[sym]);

    const prog = engine.compile('print(price("ABC") * 2);');
    expect(prog.usesHostFunctions).toBe(true);

    const r = prog.run();
    expect(r.stdout).toBe("84\n");
  });

  it("rejects runAsync for programs using host functions", async () => {
    const engine = new Engine();
    engine.registerFunction("double", (x: any) => x * 2);
    const prog = engine.compile("double(21);");

    expect(() => prog.runAsync()).toThrow();
    try {
      void prog.runAsync();
    } catch (e: any) {
      expect(e.code).toBe("ENGINE_ASYNC_HOST");
    }
  });

  it("propagates host function exceptions to run()", () => {
    const engine = new Engine();
    engine.registerFunction("fail", () => {
      throw new Error("custom host error");
    });

    const prog = engine.compile("fail();");
    expect(() => prog.run()).toThrow("custom host error");
  });
});

describe("Engine - Async Run", () => {
  it("executes asynchronously without host functions", async () => {
    const engine = new Engine();
    const prog = engine.compile(
      "let sum = 0; let i = 1; while (i <= 100) { sum = sum + i; i = i + 1; } sum;",
    );
    expect(prog.usesHostFunctions).toBe(false);
    const res = (await prog.runAsync()) as { value: any; stdout: string };
    expect(res.value).toBe(5050);
  });
});

describe("Engine - Errors & Spans", () => {
  it("provides line and column on parse errors", () => {
    const engine = new Engine();
    try {
      engine.compile("let = ;");
      expect.unreachable();
    } catch (e: any) {
      expect(e.code).toBe("ENGINE_PARSE");
      expect(e.line).toBe(1);
      expect(e.column).toBe(5);
    }
  });

  it("provides line and column on compile errors", () => {
    const engine = new Engine();
    try {
      engine.compile("let x = 1; let x = 2;");
      expect.unreachable();
    } catch (e: any) {
      expect(e.code).toBe("ENGINE_COMPILE");
      expect(e.line).toBe(1);
    }
  });
});
