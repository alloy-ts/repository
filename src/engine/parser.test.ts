import { describe, expect, test } from "vite-plus/test";
import { Engine } from "../main.js";

describe("Engine Parser", () => {
  const engine = new Engine();

  test("parses operator precedence correctly (* and / over + and -)", () => {
    // 2 + 3 * 4 should be 14, not 20
    const res1 = engine.execute("print(2 + 3 * 4);");
    expect(res1.stdout.trim()).toBe("14");

    // (2 + 3) * 4 should be 20
    const res2 = engine.execute("print((2 + 3) * 4);");
    expect(res2.stdout.trim()).toBe("20");

    // 20 - 4 / 2 should be 18
    const res3 = engine.execute("print(20 - 4 / 2);");
    expect(res3.stdout.trim()).toBe("18");
  });

  test("parses unary negation and not", () => {
    const res = engine.execute(`
      let a = -5;
      print(a + 10);
      let b = !false;
      if (b) {
        print("negation worked");
      }
    `);
    expect(res.stdout).toBe("5\nnegation worked\n");
  });

  test("parses let declaration and variable reassignment", () => {
    const res = engine.execute(`
      let x = 100;
      x = x + 50;
      print(x);
    `);
    expect(res.stdout.trim()).toBe("150");
  });

  test("parses nested block scopes and shadowing", () => {
    const res = engine.execute(`
      let x = 10;
      {
        let x = 20;
        print(x);
      }
      print(x);
    `);
    expect(res.stdout).toBe("20\n10\n");
  });

  test("parses if-else if-else chains", () => {
    const res = engine.execute(`
      let score = 75;
      if (score >= 90) {
        print("A");
      } else if (score >= 80) {
        print("B");
      } else if (score >= 70) {
        print("C");
      } else {
        print("F");
      }
    `);
    expect(res.stdout.trim()).toBe("C");
  });

  test("parses while loops with blocks", () => {
    const res = engine.execute(`
      let i = 0;
      let count = 0;
      while (i < 5) {
        count = count + 2;
        i = i + 1;
      }
      print(count);
    `);
    expect(res.stdout.trim()).toBe("10");
  });

  test("parses functions with zero, one, or multiple parameters", () => {
    const res = engine.execute(`
      fn getZero() {
        return 0;
      }
      fn inc(x) {
        return x + 1;
      }
      fn add3(a, b, c) {
        return a + b + c;
      }
      print(getZero());
      print(inc(41));
      print(add3(10, 20, 30));
    `);
    expect(res.stdout).toBe("0\n42\n60\n");
  });

  describe("Parser Error Handling", () => {
    test("throws on missing semicolon in statement", () => {
      expect(() => {
        engine.compile("let x = 10");
      }).toThrow();
    });

    test("throws on missing closing parenthesis", () => {
      expect(() => {
        engine.compile("if (x > 10 { print(x); }");
      }).toThrow();
    });

    test("throws on missing closing brace", () => {
      expect(() => {
        engine.compile("if (1) { print(1);");
      }).toThrow();
    });

    test("throws on invalid variable name in let", () => {
      expect(() => {
        engine.compile("let 123 = 456;");
      }).toThrow();
    });

    test("throws on undefined function call", () => {
      expect(() => {
        engine.compile("nonExistentFunction();");
      }).toThrow();
    });

    test("throws on function call arity mismatch", () => {
      expect(() => {
        engine.compile(`
          fn add(a, b) { return a + b; }
          add(1);
        `);
      }).toThrow(/expects 2 arguments, got 1/);
    });
  });
});
