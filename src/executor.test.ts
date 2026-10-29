import { describe, expect, test } from "vite-plus/test";
import { Executor } from "./main.ts";

// `Executor` is a unified, generic task executor that supports both task-based
// evaluation (a CPU-bound unit of work with progress + promise + cancel, backed
// by libuv's thread pool, the `futures` pool, or a single-threaded `LocalPool`)
// and future-based execution (`submit_programme`, `compute_parallel`). The
// default backend is libuv. These tests exercise the napi surface:
//
//   - `submit(input, on_progress?)` returns a `TaskHandle`.
//   - `handle.promise()` resolves with the computed result.
//   - `handle.typeName()` reports the underlying request type.
//   - `handle.cancel()` cancels the task (libuv) or is pre-start-only (futures).
//   - `runTask(options)` runs a Worker-style `Task` through the executor.
//   - `asyncWaker(cb)` returns a `TaskSignal` (`uv_async_t`) loop wakeup.
//
// NOTE: the native module under `dist/` must be rebuilt (`npm run build`) after
// the Rust bindings change for these tests to run.

describe("Executor", () => {
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
    const exec = new Executor();
    const req = exec.submit(10);
    const result = await req.promise();
    // submit(10) -> fib(11) === 89
    expect(result).toBe(fib(10 + 1));
  });

  test("submit() invokes the progress callback on the main thread", async () => {
    const exec = new Executor();
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

  test("typeName() reports the underlying libuv request type", () => {
    const exec = new Executor();
    const req = exec.submit(2);
    // Read synchronously, before the work completes; for the default libuv
    // backend this reports the underlying libuv request type.
    expect(req.typeName()).toBe("WORK");
  });

  test("cancel() is callable and does not break the result promise", async () => {
    const exec = new Executor();
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
    const exec = new Executor();
    const req = exec.submit(3);
    expect(typeof req.promise).toBe("function");
    expect(typeof req.cancel).toBe("function");
    expect(typeof req.typeName).toBe("function");
  });

  test("runTask() runs a Worker-style Task through the executor", async () => {
    const exec = new Executor();
    const task = exec.runTask({ entries: "postMessage('via-run-task');" });
    expect(typeof task.threadId).toBe("number");
    // `done()` resolves once the programme has run; `terminate()` resolves
    // with the recorded exit code.
    await expect(task.done()).resolves.toBeUndefined();
    expect(await task.terminate()).toBe(0);
  });

  test("asyncWaker() returns a uv_async_t wakeup bound to the loop", async () => {
    const exec = new Executor();
    const hits: number[] = [];
    const waker = exec.asyncWaker(() => {
      hits.push(1);
    });
    waker.send();
    // The callback is scheduled on the Node event loop, not run synchronously.
    await new Promise((r) => setTimeout(r, 20));
    expect(hits.length).toBeGreaterThan(0);
    waker.close();
  });
});
