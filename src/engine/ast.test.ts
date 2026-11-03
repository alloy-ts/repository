import { describe, expect, test } from "vite-plus/test";
import { Engine } from "../main.js";

describe("Engine AST & Evaluation", () => {
  const engine = new Engine();

  test("evaluates integer numbers and arithmetic AST nodes", () => {
    const res = engine.execute("let a = 15; let b = 4; print(a + b); print(a - b); print(a * b); print(a % b);");
    expect(res.stdout).toBe("19\n11\n60\n3\n");
  });

  test("evaluates floating-point numbers", () => {
    const res = engine.execute("let a = 5.5; let b = 2.0; print(a + b); print(a * b);");
    expect(res.stdout).toBe("7.5\n11\n");
  });

  test("evaluates string literals", () => {
    const res = engine.execute("let msg = \"Hello, FuseVM!\"; print(msg);");
    expect(res.stdout).toBe("Hello, FuseVM!\n");
  });

  test("evaluates boolean literals and expressions", () => {
    const res = engine.execute("let t = true; let f = false; if (t) { print(\"is_true\"); } if (!f) { print(\"not_false\"); }");
    expect(res.stdout).toBe("is_true\nnot_false\n");
  });

  test("evaluates binary comparisons", () => {
    const res = engine.execute(`
      let a = 10;
      let b = 20;
      if (a < b) { print("lt"); }
      if (b > a) { print("gt"); }
      if (a <= 10) { print("le"); }
      if (b >= 20) { print("ge"); }
      if (a == 10) { print("eq"); }
      if (a != b) { print("ne"); }
    `);
    expect(res.stdout).toBe("lt\ngt\nle\nge\neq\nne\n");
  });

  test("evaluates function call AST expressions", () => {
    const res = engine.execute(`
      fn multiply(x, y) {
        return x * y;
      }
      let z = multiply(6, 7);
      print(z);
    `);
    expect(res.stdout).toBe("42\n");
  });
});
