import { describe, expect, test } from "vite-plus/test";
import { Engine } from "../main.js";

describe("Engine Functions", () => {
  const engine = new Engine();

  test("declares and invokes a simple function with return value", () => {
    const res = engine.execute(`
      fn add(a, b) {
        return a + b;
      }
      let sum = add(15, 27);
      print(sum);
    `);
    expect(res.stdout.trim()).toBe("42");
  });

  test("handles functions with no parameters", () => {
    const res = engine.execute(`
      fn greetCode() {
        return 999;
      }
      print(greetCode());
    `);
    expect(res.stdout.trim()).toBe("999");
  });

  test("handles recursive functions (factorial)", () => {
    const res = engine.execute(`
      fn factorial(n) {
        if (n <= 1) {
          return 1;
        }
        return n * factorial(n - 1);
      }
      print(factorial(6));
    `);
    expect(res.stdout.trim()).toBe("720");
  });

  test("handles recursive functions (fibonacci)", () => {
    const res = engine.execute(`
      fn fib(n) {
        if (n <= 0) {
          return 0;
        }
        if (n == 1) {
          return 1;
        }
        return fib(n - 1) + fib(n - 2);
      }
      print(fib(7));
    `);
    expect(res.stdout.trim()).toBe("13");
  });

  test("supports forward references where function is called before its declaration in source", () => {
    const res = engine.execute(`
      let ans = helper(5);
      print(ans);

      fn helper(x) {
        return x * 10;
      }
    `);
    expect(res.stdout.trim()).toBe("50");
  });

  test("supports multiple nested function calls as arguments", () => {
    const res = engine.execute(`
      fn square(x) {
        return x * x;
      }
      fn add(a, b) {
        return a + b;
      }
      print(add(square(3), square(4)));
    `);
    expect(res.stdout.trim()).toBe("25");
  });

  test("preserves local variable isolation across recursive calls", () => {
    const res = engine.execute(`
      fn count(n) {
        let local = n * 10;
        if (n > 1) {
          count(n - 1);
        }
        print(local);
        return 0;
      }
      count(3);
    `);
    // When unwinding recursion, local should be 30, then 20, then 10 (or in order of execution)
    // count(3) calls count(2) which calls count(1).
    // count(1) prints 10.
    // count(2) prints 20.
    // count(3) prints 30.
    expect(res.stdout).toBe("10\n20\n30\n");
  });

  describe("Function Error Cases", () => {
    test("throws compile error when calling an undefined function", () => {
      expect(() => {
        engine.compile("computeUnknown(1, 2);");
      }).toThrow(/Undefined function/);
    });

    test("throws compile error on too few arguments", () => {
      expect(() => {
        engine.compile(`
          fn diff(a, b) { return a - b; }
          diff(10);
        `);
      }).toThrow(/expects 2 arguments, got 1/);
    });

    test("throws compile error on too many arguments", () => {
      expect(() => {
        engine.compile(`
          fn diff(a, b) { return a - b; }
          diff(10, 5, 2);
        `);
      }).toThrow(/expects 2 arguments, got 3/);
    });

    test("throws compile error on duplicate function declaration", () => {
      expect(() => {
        engine.compile(`
          fn testDup() { return 1; }
          fn testDup() { return 2; }
        `);
      }).toThrow(/Function already declared/);
    });
  });
});
