import { describe, expect, test } from "vite-plus/test";

import { negotiationWindowSize } from "./main.ts";

describe("negotiationWindowSize (src/negotiate.rs)", () => {
  test("initial window is 16 for both transport kinds", () => {
    expect(negotiationWindowSize(false, undefined)).toBe(16);
    expect(negotiationWindowSize(true, undefined)).toBe(16);
  });

  test("stateful windows grow by doubling until 32, then by 32", () => {
    expect(negotiationWindowSize(false, 16)).toBe(32);
    expect(negotiationWindowSize(false, 32)).toBe(64);
  });

  test("stateless windows double until 16384, then grow by 10%", () => {
    expect(negotiationWindowSize(true, 16)).toBe(32);
    expect(negotiationWindowSize(true, 16384)).toBe(18022);
  });
});
