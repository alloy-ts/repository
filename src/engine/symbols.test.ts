import { describe, expect, test } from "vite-plus/test";
import { Engine } from "../main.js";

describe("Engine Symbols & Scoping", () => {
  const engine = new Engine();

  test("declares and accesses global scope symbols", () => {
    const res = engine.execute(`
      let a = 10;
      let b = 20;
      let c = a + b;
      print(c);
    `);
    expect(res.stdout.trim()).toBe("30");
  });

  test("reassigns variables in the same scope", () => {
    const res = engine.execute(`
      let val = 1;
      val = 2;
      val = val + 3;
      print(val);
    `);
    expect(res.stdout.trim()).toBe("5");
  });

  test("allows variable shadowing in nested block scopes", () => {
    const res = engine.execute(`
      let x = 100;
      {
        let x = 200;
        print(x);
        {
          let x = 300;
          print(x);
        }
        print(x);
      }
      print(x);
    `);
    expect(res.stdout).toBe("200\n300\n200\n100\n");
  });

  test("allows inner scopes to read outer scope variables without shadowing", () => {
    const res = engine.execute(`
      let outer = 42;
      {
        let inner = 8;
        print(outer + inner);
      }
      print(outer);
    `);
    expect(res.stdout).toBe("50\n42\n");
  });

  test("allows inner scopes to modify outer scope variables", () => {
    const res = engine.execute(`
      let counter = 0;
      {
        counter = counter + 10;
        {
          counter = counter + 5;
        }
      }
      print(counter);
    `);
    expect(res.stdout.trim()).toBe("15");
  });

  test("handles loop scope isolation across iterations", () => {
    const res = engine.execute(`
      let sum = 0;
      let i = 0;
      while (i < 3) {
        let step = i * 2;
        sum = sum + step;
        i = i + 1;
      }
      print(sum);
    `);
    // step = 0, then 2, then 4 -> sum = 6
    expect(res.stdout.trim()).toBe("6");
  });

  describe("Symbol Error Cases", () => {
    test("throws compile error when reading an undefined variable", () => {
      expect(() => {
        engine.compile("print(undeclaredVar);");
      }).toThrow(/Undefined variable/);
    });

    test("throws compile error when reassigning an undefined variable", () => {
      expect(() => {
        engine.compile("undeclaredVar = 10;");
      }).toThrow(/Undefined variable/);
    });

    test("throws compile error when declaring duplicate variable in same scope", () => {
      expect(() => {
        engine.compile(`
          let duplicate = 1;
          let duplicate = 2;
        `);
      }).toThrow(/Variable already declared in current scope/);
    });

    test("throws compile error when accessing an inner block variable from outer scope", () => {
      expect(() => {
        engine.compile(`
          {
            let blockScoped = 99;
          }
          print(blockScoped);
        `);
      }).toThrow(/Undefined variable/);
    });
  });
});
