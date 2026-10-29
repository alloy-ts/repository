import { describe, expect, test } from "vite-plus/test";
import { TaskExecutor } from "./main.ts";

// `TaskExecutor` runs CPU-bound work on libuv's thread pool via `uv_queue_work`
// and reports progress back to the JS (V8) thread through `uv_async_send`. These
// tests exercise the napi surface:
//
//   - `submit(input, on_progress?)` returns a `TaskReq` handle.
//   - `taskReq.promise()` resolves with the computed result.
//   - `taskReq.type_name()` reports the underlying libuv request type.
//   - `taskReq.cancel()` cancels the task (only while it is still pending).
//
// NOTE: the native module under `dist/` must be rebuilt (`napi build`) after the
// `TaskExecutor`/`TaskReq` Rust bindings were added for these tests to run.

describe("TaskExecutor", () => {
  // `submit(n)` computes the (n+1)-th Fibonacci number (fib(0) = 0, fib(1) = 1).
  const fib = (n: number): number => {
    let a = 0;
    let b = 1;
    for (let i = 0; i < n; i++) {
      const c = a + b;
      a = b;
      b = c;
    }
    return b;
  };

  test("submit() resolves the result promise with the computed value", async () => {
    const exec = new TaskExecutor();
    const req = exec.submit(10);
    const result = await req.promise();
    // submit(10) -> fib(11) === 89
    expect(result).toBe(fib(10 + 1));
  });

  test("submit() invokes the progress callback on the main thread", async () => {
    const exec = new TaskExecutor();
    const seen: number[] = [];
    const req = exec.submit(10, (pct: number) => {
      seen.push(pct);
    });
    const result = await req.promise();
    expect(result).toBe(fib(11));

    // Progress is reported from 0 up to 100, monotonically non-decreasing.
    expect(seen.length).toBeGreaterThan(0);
    expect(seen[seen.length - 1]).toBe(100);
    for (let i = 1; i < seen.length; i++) {
      expect(seen[i]).toBeGreaterThanOrEqual(seen[i - 1]);
    }
  });

  test("type_name() reports the underlying libuv request type", () => {
    const exec = new TaskExecutor();
    const req = exec.submit(2);
    // Read synchronously, before the work completes: libuv frees the request
    // once the after-work callback runs, so this must be called while live.
    expect(req.type_name()).toBe("WORK");
  });

  test("cancel() is callable and does not break the result promise", async () => {
    const exec = new TaskExecutor();
    // A large input keeps the task alive long enough to attempt cancellation.
    const req = exec.submit(90);
    try {
      req.cancel();
    } catch {
      // If the task already started, libuv rejects the cancel with an error,
      // surfaced here as a JS exception; either way the call is safe.
    }
    const result = await req.promise();
    expect(typeof result).toBe("number");
  });

  test("exposes the expected handle methods", () => {
    const exec = new TaskExecutor();
    const req = exec.submit(3);
    expect(typeof req.promise).toBe("function");
    expect(typeof req.cancel).toBe("function");
    expect(typeof req.type_name).toBe("function");
  });
});
