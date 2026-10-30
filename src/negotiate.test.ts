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

  test("computeNegotiationRound calculates round stats", async () => {
    const { computeNegotiationRound } = await import("./main.ts");
    const round1 = computeNegotiationRound(16, 0, 32, false);
    expect(round1.havesSent).toBe(16);
    expect(round1.inVain).toBe(16);
    expect(round1.havesToSend).toBe(32);
    expect(round1.previousResponseHadAtLeastOneInCommon).toBe(false);

    const round2 = computeNegotiationRound(32, 16, 64, true);
    expect(round2.inVain).toBe(0);
    expect(round2.previousResponseHadAtLeastOneInCommon).toBe(true);
  });
});
