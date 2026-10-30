import { writeFileSync, unlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, test } from "vite-plus/test";
import {
  Task,
  isMainThread,
  getEnvMessagePort,
  K_MAX_YOUNG_GENERATION_SIZE_MB,
  K_MAX_OLD_GENERATION_SIZE_MB,
  K_CODE_RANGE_SIZE_MB,
  K_STACK_SIZE_MB,
  K_TOTAL_RESOURCE_LIMIT_COUNT,
} from "./main.ts";

describe("Threaded Tasks API (src/task.rs)", () => {
  test("constants and main thread helpers", () => {
    expect(isMainThread()).toBe(true);
    expect(getEnvMessagePort()).toBeNull();
    expect(K_MAX_YOUNG_GENERATION_SIZE_MB).toBe(0);
    expect(K_MAX_OLD_GENERATION_SIZE_MB).toBe(1);
    expect(K_CODE_RANGE_SIZE_MB).toBe(2);
    expect(K_STACK_SIZE_MB).toBe(3);
    expect(K_TOTAL_RESOURCE_LIMIT_COUNT).toBe(4);
  });

  test("Task instantiation, postMessage, onmessage, and terminate", async () => {
    const scriptPath = join(tmpdir(), `task-test-${Date.now()}-${Math.random().toString(36).slice(2)}.js`);
    const code = `
      const { parentPort } = require('node:worker_threads');
      parentPort.on('message', (msg) => {
        parentPort.postMessage({ echo: msg });
      });
    `;
    writeFileSync(scriptPath, code);

    try {
      const task = new Task(scriptPath);
      expect(typeof task.threadId).toBe("number");
      expect(task.threadId).toBeGreaterThan(0);
      expect(task.terminated).toBe(false);

      const responsePromise = new Promise<any>((resolve) => {
        task.onmessage = (e: any) => {
          resolve(e.data);
        };
      });

      task.postMessage("hello from main thread");
      const received = await responsePromise;
      expect(received).toEqual({ echo: "hello from main thread" });

      await task.terminate();
      expect(task.terminated).toBe(true);
    } finally {
      try { unlinkSync(scriptPath); } catch {}
    }
  });

  test("Task with eval option and addEventListener", async () => {
    const evalScript = `
      const { parentPort } = require('node:worker_threads');
      parentPort.on('message', (msg) => {
        parentPort.postMessage([msg[0] + msg[1]]);
      });
    `;

    const task = new Task(evalScript, { eval: true, name: "eval-task" });
    expect(task.threadId).toBeGreaterThan(0);

    const receivedPromise = new Promise<any>((resolve) => {
      const listener = (e: any) => {
        task.removeEventListener("message", listener);
        resolve(e.data);
      };
      task.addEventListener("message", listener);
    });

    task.postMessage([10, 20]);
    const res = await receivedPromise;
    expect(res).toEqual([30]);

    await task.terminate();
  });

  test("Task ref, unref, and dispatchEvent", async () => {
    const task = new Task("parentPort.postMessage('ready')", { eval: true });

    // Test ref and unref calls
    task.unref();
    task.ref();
    task.unreference();
    task.reference();

    // Test synthetic dispatchEvent
    let customEventFired = false;
    task.addEventListener("custom", (e: any) => {
      if (e.detail === 42) customEventFired = true;
    });

    const dispatched = task.dispatchEvent({ type: "custom", detail: 42 });
    expect(dispatched).toBe(true);
    expect(customEventFired).toBe(true);

    await task.terminate();
  });
});
