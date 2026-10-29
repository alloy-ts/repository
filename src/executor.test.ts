import { describe, expect, test } from "vite-plus/test";
import { Executor, ExecutorKind } from "./main.ts";

// `Executor` is a unified, generic task executor that supports both task-based
// evaluation (a CPU-bound unit of work with progress + promise + cancel, backed
// by libuv's thread pool, the `futures` pool, or a single-threaded `LocalPool`)
// and future-based execution (`submit_async`, `submit_work`,
// `compute_parallel`). The default backend is libuv. These tests exercise the
// napi surface:
//
//   - `submit(input, on_progress?)` returns a `TaskHandle` with
//     `promise()` / `cancel()` / `type_name()`.
//   - `submit_async(input)` resolves the value directly (tokio_rt-driven future).
//   - `submit_work(src)` evaluates a JS work and resolves its result.
//   - `compute_parallel(a, b)` runs two fibs concurrently (`futures::join!`).
//   - `spawn(cb)` fire-and-forgets a callback onto the scheduler.
//
// NOTE: the native module under `dist/` must be rebuilt (`npm run build`) after
// the Rust bindings change for these tests to run.

// `submit(n)` / `submit_async(n)` compute fib(n + 1) (fib(0) = 0, fib(1) = 1);
// `compute_parallel(a, b)` resolves `[fib(a + 1), fib(b + 1)]`.
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

describe("Executor (task-based path)", () => {
  test("submit() resolves the result promise with the computed value", async () => {
    const exec = new Executor();
    const req = exec.submit(10);
    // submit(10) -> fib(11) === 89
    expect(await req.promise()).toBe(fib(11));
  });

  test("submit() invokes the progress callback on the main thread", async () => {
    const exec = new Executor();
    const seen: number[] = [];
    const req = exec.submit(10, (pct: number) => {
      seen.push(pct);
    });
    expect(await req.promise()).toBe(fib(11));

    // Progress is reported from 0 up to 100, monotonically non-decreasing.
    expect(seen.length).toBeGreaterThan(0);
    expect(seen[seen.length - 1]).toBe(100);
    for (let i = 1; i < seen.length; i++) {
      expect(seen[i]).toBeGreaterThanOrEqual(seen[i - 1]);
    }
  });

  test("type_name() reports the underlying libuv request type", () => {
    const exec = new Executor();
    const req = exec.submit(2);
    // Read synchronously, before the work completes; for the default libuv
    // backend this reports the underlying libuv request type.
    expect(req.type_name()).toBe("WORK");
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
    expect(typeof (await req.promise())).toBe("number");
  });

  test("exposes the expected handle methods", () => {
    const exec = new Executor();
    const req = exec.submit(3);
    expect(typeof req.promise).toBe("function");
    expect(typeof req.cancel).toBe("function");
    expect(typeof req.type_name).toBe("function");
  });

  test("spawn() invokes the callback on the event loop", async () => {
    const exec = new Executor();
    await new Promise<void>((resolve) => exec.spawn(resolve));
  });
});

describe("Executor (future-based path)", () => {
  test("submit_async() resolves the value directly", async () => {
    const exec = new Executor();
    expect(await exec.submit_async(10)).toBe(fib(11));
  });

  test("submit_work() evaluates a JS work and resolves its result", async () => {
    const exec = new Executor();
    expect(await exec.submit_work("return 1 + 41")).toBe(42);
  });

  test("compute_parallel() runs two computations concurrently", async () => {
    const exec = new Executor();
    expect(await exec.compute_parallel(10, 5)).toEqual([fib(11), fib(6)]);
  });
});

describe("Executor (alternative backends)", () => {
  test("Futures backend: submit() resolves via the futures thread pool", async () => {
    const exec = new Executor({ kind: ExecutorKind.Futures });
    expect(await (await exec.submit(5)).promise()).toBe(fib(6));
  });

  test("Local backend: submit() resolves via the single-threaded LocalPool", async () => {
    const exec = new Executor({ kind: ExecutorKind.Local });
    const req = exec.submit(5);
    expect(req.type_name()).toBe("LOCAL_TASK");
    expect(await req.promise()).toBe(fib(6));
  });
});
