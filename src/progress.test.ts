import { describe, expect, test } from "vite-plus/test";

import { RemoteProgress } from "./main.ts";

describe("RemoteProgress.fromBytes", () => {
  test("parses action, counts, and throughput from a transfer line", () => {
    const p = RemoteProgress.fromBytes("Receiving objects:  50% (100/200), 1.23 MiB | 4.56 MiB/s");
    expect(p).not.toBeNull();
    expect(p!.action).toBe("Receiving objects");
    expect(p!.percent).toBe(50);
    expect(p!.step).toBe(100);
    expect(p!.max).toBe(200);
    // 1.23 MiB = 1.23 * 1024^2
    expect(p!.receivedBytes).toBeCloseTo(1.23 * 1024 * 1024, 2);
    // 4.56 MiB/s = 4.56 * 1024^2
    expect(p!.throughput).toBeCloseTo(4.56 * 1024 * 1024, 2);
  });

  test("omits byte fields for lines without throughput", () => {
    const p = RemoteProgress.fromBytes("Resolving deltas: 30% (30/100)");
    expect(p!.action).toBe("Resolving deltas");
    expect(p!.percent).toBe(30);
    expect(p!.step).toBe(30);
    expect(p!.max).toBe(100);
    expect(p!.receivedBytes).toBeNull();
    expect(p!.throughput).toBeNull();
  });

  test("returns null for non-progress lines", () => {
    expect(RemoteProgress.fromBytes("")).toBeNull();
    expect(RemoteProgress.fromBytes("To https://example.com/repo.git")).toBeNull();
  });
});
