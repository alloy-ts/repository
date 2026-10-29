import { writeFileSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, test, afterAll, beforeAll } from "vite-plus/test";
import {
  Task,
  TaskType,
  DedicatedTaskGlobalScope,
  SharedTaskGlobalScope,
  ServiceTaskGlobalScope,
  isMainThread,
  getThreadId,
  setEnvironmentData,
  getEnvironmentData,
  createMainThreadPort,
  destroyMainThreadPort,
} from "../dist/index.js";

describe("(Threaded) Tasks API", () => {
  let tmpDir: string;
  let workerScript: string;

  beforeAll(() => {
    tmpDir = mkdtempSync(join(tmpdir(), "task-test-"));
    workerScript = join(tmpDir, "worker.cjs");
    const code = `
      const { parentPort } = require('node:worker_threads');
      if (parentPort) {
        parentPort.on('message', (msg) => {
          if (Array.isArray(msg)) {
            const sum = msg.reduce((a, b) => Number(a) + Number(b), 0);
            parentPort.postMessage({ result: sum, echo: msg });
          } else if (msg && msg.action === 'ping') {
            parentPort.postMessage('pong');
          } else {
            parentPort.postMessage(msg);
          }
        });
      }
    `;
    writeFileSync(workerScript, code, "utf8");
  });

  afterAll(() => {
    try {
      rmSync(tmpDir, { recursive: true, force: true });
    } catch {}
  });

  test("main thread detection and thread ID", () => {
    expect(isMainThread()).toBe(true);
    expect(getThreadId()).toBeGreaterThanOrEqual(0);
  });

  test("Task instantiation and thread properties", () => {
    const task = new Task(workerScript, { name: "test-task" });
    expect(task.script).toBe(workerScript);
    expect(typeof task.threadId).toBe("number");
    expect(task.threadName).toBe("test-task");
    expect(task.terminated).toBe(false);
    task.terminate();
    expect(task.terminated).toBe(true);
  });

  test("postMessage and onmessage event handling", async () => {
    const task = new Task(workerScript);
    const received: any[] = [];

    await new Promise<void>((resolve, reject) => {
      const timeout = setTimeout(() => {
        reject(new Error("Timed out waiting for task onmessage"));
      }, 5000);

      task.onmessage = (e: any) => {
        received.push(e.data);
        if (received.length === 2) {
          clearTimeout(timeout);
          resolve();
        }
      };

      task.postMessage({ action: "ping" });
      task.postMessage([10, 20]);
    });

    expect(received[0]).toBe("pong");
    expect(received[1]).toEqual({ result: 30, echo: [10, 20] });
    task.terminate();
  });

  test("addEventListener and removeEventListener", async () => {
    const task = new Task(workerScript);
    let eventData: any = null;

    await new Promise<void>((resolve, reject) => {
      const timeout = setTimeout(() => {
        reject(new Error("Timed out waiting for addEventListener message"));
      }, 5000);

      const listener = (e: any) => {
        eventData = e.data;
        clearTimeout(timeout);
        resolve();
      };

      task.addEventListener("message", listener);
      task.postMessage({ action: "ping" });
    });

    expect(eventData).toBe("pong");

    let secondCalled = false;
    const secondListener = () => {
      secondCalled = true;
    };
    task.addEventListener("message", secondListener);
    task.removeEventListener("message", secondListener);

    task.terminate();
    expect(secondCalled).toBe(false);
  });

  test("dispatchEvent manually fires listeners", () => {
    const task = new Task(workerScript);
    let dispatched = false;

    task.addEventListener("custom", (e: any) => {
      if (e && e.detail === "test") {
        dispatched = true;
      }
    });

    task.dispatchEvent({ type: "custom", detail: "test" });
    expect(dispatched).toBe(true);
    task.terminate();
  });

  test("ref and unref methods", () => {
    const task = new Task(workerScript);
    expect(() => task.unref()).not.toThrow();
    expect(() => task.ref()).not.toThrow();
    task.terminate();
  });

  test("environment data set and get", () => {
    setEnvironmentData("key1", "value1");
    expect(getEnvironmentData("key1")).toBe("value1");
    setEnvironmentData("key1", undefined);
    expect(getEnvironmentData("key1")).toBeUndefined();
  });

  test("main thread port creation and destruction", () => {
    const port = createMainThreadPort(999);
    expect(port).toBeDefined();
    expect(() => destroyMainThreadPort(999)).not.toThrow();
  });

  test("Task scope constructors and enum exist", () => {
    expect(TaskType.Dedicated).toBeDefined();
    expect(TaskType.Shared).toBeDefined();
    expect(TaskType.Service).toBeDefined();
    expect(DedicatedTaskGlobalScope).toBeDefined();
    expect(SharedTaskGlobalScope).toBeDefined();
    expect(ServiceTaskGlobalScope).toBeDefined();
  });
});
