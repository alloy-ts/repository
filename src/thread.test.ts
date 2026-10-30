import { describe, expect, test } from "vite-plus/test";
import { Builder, JoinHandle, Thread, ThreadPool } from "./main.ts";

describe("Thread (src/thread.rs)", () => {
  test("Thread.current() returns the current thread handle", () => {
    const thread = Thread.current();
    expect(thread).toBeInstanceOf(Thread);
    expect(typeof thread.id).toBe("number");
    expect(thread.id).toBeGreaterThan(0);
    expect(thread.name === null || typeof thread.name === "string").toBe(true);
  });

  test("Thread static methods sleep, yieldNow, parkTimeout, unpark", () => {
    const start = Date.now();
    Thread.sleep(10);
    const elapsed = Date.now() - start;
    expect(elapsed).toBeGreaterThanOrEqual(5);

    expect(() => Thread.yieldNow()).not.toThrow();

    const thread = Thread.current();
    expect(() => thread.unpark()).not.toThrow();
    expect(() => Thread.parkTimeout(5)).not.toThrow();
  });

  test("Builder configures and spawns a thread", async () => {
    const builder = Builder.new();
    const namedBuilder = builder.name("test-worker-thread").stackSize(2 * 1024 * 1024);
    expect(namedBuilder).toBeInstanceOf(Builder);

    let executed = false;
    const handle = namedBuilder.spawn(() => {
      executed = true;
    });

    expect(handle).toBeInstanceOf(JoinHandle);
    const spawnedThread = handle.thread();
    expect(spawnedThread).toBeInstanceOf(Thread);
    expect(spawnedThread.name).toBe("test-worker-thread");

    await new Promise<void>((resolve) => {
      handle.joinAsync(() => {
        resolve();
      });
    });

    expect(executed).toBe(true);
  });

  test("JoinHandle joinAsync waits for thread completion", async () => {
    const builder = Builder.new();
    let counter = 0;
    const handle = builder.spawn(() => {
      counter += 42;
    });

    await new Promise<void>((resolve) => {
      handle.joinAsync(() => {
        resolve();
      });
    });

    expect(counter).toBe(42);
  });
});

describe("ThreadPool (src/thread.rs)", () => {
  test("static size accessors return expected values", () => {
    expect(ThreadPool.maxSize()).toBe(1024);
    expect(ThreadPool.defaultSize()).toBe(4);
    expect(typeof ThreadPool.size()).toBe("number");
    expect(ThreadPool.size()).toBeGreaterThanOrEqual(1);
    expect(ThreadPool.size()).toBeLessThanOrEqual(ThreadPool.maxSize());
  });

  test("constructor creates a ThreadPool handle bound to the loop", () => {
    const pool = new ThreadPool();
    expect(pool).toBeInstanceOf(ThreadPool);
  });
});
