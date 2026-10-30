import { describe, expect, test } from "vite-plus/test";
import { Builder, JoinHandle, Thread, ThreadPool } from "./main.ts";

// `src/thread.rs` exports:
//
//   - `Thread` — wraps `std::thread::Thread`, exposing `current()`, `id`, `name`,
//     `unpark()`, `sleep()`, `yieldNow()`, `park()`, `parkTimeout()`.
//   - `Builder` — mirrors `std::thread::Builder`, configuring `name` and `stackSize`
//     before spawning a thread with `spawn(task)`.
//   - `JoinHandle` — owned handle to join a spawned thread synchronously (`join()`)
//     or asynchronously (`joinAsync(done)`).
//   - `ThreadPool` — handle to libuv's global thread pool exposing `maxSize()`,
//     `defaultSize()`, and `size()`.

describe("Thread (src/thread.rs)", () => {
  test("Thread.current() returns a handle to the current thread", () => {
    const thread = Thread.current();
    expect(thread).toBeInstanceOf(Thread);
    expect(typeof thread.id).toBe("number");
    expect(thread.id).toBeGreaterThan(0);
    expect(thread.name === null || typeof thread.name === "string").toBe(true);
  });

  test("Thread.sleep pauses execution", () => {
    const start = Date.now();
    Thread.sleep(20);
    const elapsed = Date.now() - start;
    expect(elapsed).toBeGreaterThanOrEqual(15);
  });

  test("Thread.yieldNow executes without error", () => {
    expect(() => Thread.yieldNow()).not.toThrow();
  });

  test("Thread.parkTimeout executes without error", () => {
    expect(() => Thread.parkTimeout(5)).not.toThrow();
  });

  test("thread.unpark executes without error", () => {
    const thread = Thread.current();
    expect(() => thread.unpark()).not.toThrow();
  });
});

describe("Builder & JoinHandle (src/thread.rs)", () => {
  test("Builder.new creates a builder with configurable name and stack size", () => {
    const builder = Builder.new();
    expect(builder).toBeInstanceOf(Builder);

    const configured = builder.name("worker-test").stackSize(2 * 1024 * 1024);
    expect(configured).toBeInstanceOf(Builder);
  });

  test("spawn runs task and joinAsync calls completion callback", async () => {
    let taskRan = false;
    let completed = false;

    const builder = Builder.new().name("async-thread");
    const handle = builder.spawn(() => {
      taskRan = true;
    });

    expect(handle).toBeInstanceOf(JoinHandle);
    const spawnedThread = handle.thread();
    expect(spawnedThread).toBeInstanceOf(Thread);
    expect(spawnedThread.name).toBe("async-thread");

    await new Promise<void>((resolve) => {
      handle.joinAsync(() => {
        completed = true;
        resolve();
      });
    });

    expect(taskRan).toBe(true);
    expect(completed).toBe(true);
  });

  test("joining an already joined handle throws an error", async () => {
    const builder = Builder.new();
    const handle = builder.spawn(() => {});

    await new Promise<void>((resolve) => {
      handle.joinAsync(() => resolve());
    });

    expect(() => handle.join()).toThrow("thread already joined");
    expect(() => handle.joinAsync(() => {})).toThrow("thread already joined");
  });
});

describe("ThreadPool (src/thread.rs)", () => {
  test("ThreadPool constructor creates an instance bound to default loop", () => {
    const pool = new ThreadPool();
    expect(pool).toBeInstanceOf(ThreadPool);
  });

  test("ThreadPool exposes size constants and current pool size", () => {
    expect(ThreadPool.maxSize()).toBe(1024);
    expect(ThreadPool.defaultSize()).toBe(4);
    expect(typeof ThreadPool.size()).toBe("number");
    expect(ThreadPool.size()).toBeGreaterThanOrEqual(1);
    expect(ThreadPool.size()).toBeLessThanOrEqual(1024);
  });
});
