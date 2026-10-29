import { Buffer } from "node:buffer";
import { describe, expect, test } from "vite-plus/test";
import { asyncPlus100, processBuffer, processBufferOnPool, promiseCallback } from "./main.ts";

// `src/promise.rs` exercises napi's Promise machinery:
//
//   - `asyncPlus100` — awaits a JS `Promise` in Rust (`Promise<T>` is a
//     `Future`) and returns a new JS `Promise` from the `#[napi] async fn`.
//   - `promiseCallback` — attaches `.then` to a `PromiseRaw`, settling a
//     `JsDeferred` with the transformed value.
//   - `processBuffer` — returns an `AsyncBlock<Buffer>` (a JS `Promise<Buffer>`
//     built from a Rust future).
//   - `processBufferOnPool` — runs the work on the libuv thread pool
//     (`uv_queue_work`), wakes the loop with a `uv_async_t` and settles a
//     `JsDeferred`: the shared worker→loop delivery primitive `task.rs` uses.
//
// NOTE: the native module under `dist/` must be rebuilt (`npm run build`) after
// the Rust bindings change for these tests to run.

describe("asyncPlus100", () => {
  test("awaits a resolved Promise and adds 100", async () => {
    expect(await asyncPlus100(Promise.resolve(23))).toBe(123);
  });

  test("propagates a rejected Promise", async () => {
    await expect(asyncPlus100(Promise.reject(new Error("nope")))).rejects.toThrow("nope");
  });
});

describe("promiseCallback", () => {
  test("resolves with the promised value transformed (+100)", async () => {
    expect(await promiseCallback(Promise.resolve(7))).toBe(107);
  });
});

describe("processBuffer", () => {
  test("echoes a Buffer back as a Promise<Buffer>", async () => {
    const input = Buffer.from([1, 2, 3, 4, 5]);
    const out = await processBuffer(input);
    expect(Buffer.isBuffer(out)).toBe(true);
    expect(out.equals(input)).toBe(true);
  });
});

describe("processBufferOnPool", () => {
  test("runs on the libuv pool and resolves a Promise<Buffer>", async () => {
    const input = Buffer.from("pool");
    const out = await processBufferOnPool(input);
    expect(Buffer.isBuffer(out)).toBe(true);
    expect(out.equals(input)).toBe(true);
  });
});
