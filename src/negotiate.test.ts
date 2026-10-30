import { describe, expect, test } from "vite-plus/test";
import {
  ActionKind,
  addWants,
  makeRefmappingIgnorePredicate,
  markCompleteAndCommonRef,
  NegotiationState,
  oneRound,
} from "./main.ts";

describe("Negotiation NAPI Bindings (src/negotiate.rs)", () => {
  test("NegotiationState constructor and window size adjustment", () => {
    const stateless = new NegotiationState(true);
    expect(stateless.isStateless).toBe(true);
    expect(stateless.havesToSend).toBe(32);

    const stateful = new NegotiationState(false);
    expect(stateful.isStateless).toBe(false);
    expect(stateful.havesToSend).toBe(16);

    stateful.seenAck = true;
    stateful.adjustWindowSize();
    expect(stateful.havesToSend).toBe(32);
  });

  test("markCompleteAndCommonRef handles edge cases and negotiation requirement", () => {
    const noChange = markCompleteAndCommonRef(0, false, 0);
    expect(noChange.kind).toBe(ActionKind.NoChange);

    const unborn = markCompleteAndCommonRef(1, true, 0);
    expect(unborn.kind).toBe(ActionKind.SkipToRefUpdate);

    const noDiff = markCompleteAndCommonRef(2, false, 0);
    expect(noDiff.kind).toBe(ActionKind.NoChange);

    const negotiate = markCompleteAndCommonRef(2, false, 1, [true, false]);
    expect(negotiate.kind).toBe(ActionKind.MustNegotiate);
    expect(negotiate.remoteRefTargetKnown).toEqual([true, false]);
  });

  test("makeRefmappingIgnorePredicate identifies tag refspecs when tags included", () => {
    expect(makeRefmappingIgnorePredicate(true, true)).toBe(true);
    expect(makeRefmappingIgnorePredicate(true, false)).toBe(false);
    expect(makeRefmappingIgnorePredicate(false, true)).toBe(false);
  });

  test("addWants computes wanted mapping indices", () => {
    const known = [true, false, false];
    const wantsNotShallow = addWants(known, false);
    expect(wantsNotShallow).toEqual([1, 2]);

    const wantsShallow = addWants(known, true);
    expect(wantsShallow).toEqual([0, 1, 2]);

    const wantsWithMask = addWants(known, false, [false, true, false]);
    expect(wantsWithMask).toEqual([2]);
  });

  test("oneRound progresses state and computes round metrics", () => {
    const state = new NegotiationState(true);
    const round1 = oneRound(state, false, 50);
    expect(round1.havesSent).toBe(32);
    expect(round1.inVain).toBe(32);
    expect(round1.previousResponseHadAtLeastOneInCommon).toBe(false);

    const round2 = oneRound(state, true, 20);
    expect(round2.previousResponseHadAtLeastOneInCommon).toBe(true);
    expect(round2.havesSent).toBe(20);
  });
});
