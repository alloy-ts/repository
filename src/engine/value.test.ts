import { describe, expect, test } from "vite-plus/test";
import { Engine } from "../../dist/index.mjs";

describe("Engine Value Mapping and Edge Cases (Phase 4)", () => {
  const engine = new Engine();

  test("primitive types round-trip via globals and return values", () => {
    const prog = engine.compile("x;", ["x"]);

    // Undefined
    expect(prog.run({ x: undefined }).value).toBe(undefined);

    // Null (lowers to Undef in FuseVM value model)
    expect(prog.run({ x: null }).value).toBe(undefined);

    // Booleans
    expect(prog.run({ x: true }).value).toBe(true);
    expect(prog.run({ x: false }).value).toBe(false);

    // Strings
    expect(prog.run({ x: "hello world" }).value).toBe("hello world");
    expect(prog.run({ x: "" }).value).toBe("");

    // Small integers
    expect(prog.run({ x: 0 }).value).toBe(0);
    expect(prog.run({ x: 42 }).value).toBe(42);
    expect(prog.run({ x: -100 }).value).toBe(-100);

    // Floating point numbers
    expect(prog.run({ x: 3.14159 }).value).toBeCloseTo(3.14159);
    expect(prog.run({ x: -0.5 }).value).toBeCloseTo(-0.5);

    // Special floats: NaN, Infinity, -Infinity
    expect(Number.isNaN(prog.run({ x: NaN }).value as number)).toBe(true);
    expect(prog.run({ x: Infinity }).value).toBe(Infinity);
    expect(prog.run({ x: -Infinity }).value).toBe(-Infinity);

    // Negative zero
    const negZeroRes = prog.run({ x: -0 }).value as number;
    expect(Object.is(negZeroRes, -0)).toBe(true);
  });

  test("arrays and nested arrays", () => {
    const prog = engine.compile("x;", ["x"]);

    // Empty array
    expect(prog.run({ x: [] }).value).toEqual([]);

    // Array of numbers and strings
    expect(prog.run({ x: [1, 2, "three", true] }).value).toEqual([1, 2, "three", true]);

    // Nested arrays
    const nested = [
      [1, 2],
      [3, [4, 5]],
    ];
    expect(prog.run({ x: nested }).value).toEqual(nested);
  });

  test("objects and nested structures", () => {
    const prog = engine.compile("x;", ["x"]);

    // Empty object
    expect(prog.run({ x: {} }).value).toEqual({});

    // Flat object
    expect(prog.run({ x: { a: 1, b: "two", c: false } }).value).toEqual({
      a: 1,
      b: "two",
      c: false,
    });

    // Nested object
    const nestedObj = {
      foo: {
        bar: 123,
        baz: ["a", "b"],
      },
    };
    expect(prog.run({ x: nestedObj }).value).toEqual(nestedObj);
  });

  test("BigInt rules around 2^53 boundary", () => {
    const prog = engine.compile("x;", ["x"]);

    // Safe integer within 2^53 - 1 passed as JS number returns JS number
    const maxSafe = Number.MAX_SAFE_INTEGER; // 9007199254740991
    expect(prog.run({ x: maxSafe }).value).toBe(maxSafe);
    expect(typeof prog.run({ x: maxSafe }).value).toBe("number");

    const minSafe = Number.MIN_SAFE_INTEGER; // -9007199254740991
    expect(prog.run({ x: minSafe }).value).toBe(minSafe);
    expect(typeof prog.run({ x: minSafe }).value).toBe("number");

    // Passing BigInt directly
    expect(prog.run({ x: 42n }).value).toBe(42);

    // BigInt above 2^53 boundary returns BigInt
    const aboveSafe = 9007199254740992n; // 2^53
    const resAbove = prog.run({ x: aboveSafe }).value;
    expect(typeof resAbove).toBe("bigint");
    expect(resAbove).toBe(aboveSafe);

    // Negative BigInt below -2^53 boundary returns BigInt
    const belowSafe = -9007199254740992n;
    const resBelow = prog.run({ x: belowSafe }).value;
    expect(typeof resBelow).toBe("bigint");
    expect(resBelow).toBe(belowSafe);
  });

  test("unsupported types and error handling", () => {
    const prog = engine.compile("x;", ["x"]);

    // Symbols cannot be passed as values
    expect(() => prog.run({ x: Symbol("test") })).toThrow("Symbols are not supported as values");

    // Functions cannot be passed as values in globals
    expect(() => prog.run({ x: () => {} })).toThrow(
      "Functions cannot be passed directly as values",
    );

    // Object containing a function throws
    expect(() => prog.run({ x: { fn: () => 123 } })).toThrow(
      "Functions cannot be passed directly as values",
    );

    // Cyclic object references throw
    const cycle: Record<string, any> = { id: 1 };
    cycle["self"] = cycle;
    expect(() => prog.run({ x: cycle })).toThrow("Cyclic object references are not supported");

    // Cyclic array references throw
    const arrCycle: any[] = [1, 2];
    arrCycle.push(arrCycle);
    expect(() => prog.run({ x: arrCycle })).toThrow("Cyclic object references are not supported");
  });
});
