import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";

import {
  CODE_RANGE_SIZE_MB,
  MAX_OLD_GENERATION_SIZE_MB,
  MAX_YOUNG_GENERATION_SIZE_MB,
  STACK_SIZE_MB,
  TOTAL_RESOURCE_LIMIT_COUNT,
  Task,
  getResourceLimits,
  isInternalThread,
  isMainThread,
  ownsProcessState,
  threadId,
  threadName,
} from "./main.ts";

let tmpDir: string;
let workerScriptPath: string;

beforeAll(() => {
  tmpDir = mkdtempSync(join(tmpdir(), "task-test-"));
  workerScriptPath = join(tmpDir, "worker.js");
  writeFileSync(
    workerScriptPath,
    `
const { parentPort } = require('node:worker_threads');
if (parentPort) {
  parentPort.on('message', (msg) => {
    if (msg === 'ping') {
      parentPort.postMessage('pong');
    } else if (Array.isArray(msg)) {
      parentPort.postMessage(msg.join('-'));
    }
  });
}
`,
  );
});

afterAll(() => {
  rmSync(tmpDir, { recursive: true, force: true });
});

describe("Task API (src/task.rs)", () => {
  test("top-level functions and constants", () => {
    expect(isMainThread()).toBe(true);
    expect(isInternalThread()).toBe(false);
    expect(ownsProcessState()).toBe(true);
    expect(typeof threadId()).toBe("number");
    expect(typeof threadName()).toBe("string");
    expect(Array.isArray(getResourceLimits())).toBe(true);

    expect(MAX_YOUNG_GENERATION_SIZE_MB).toBe(0);
    expect(MAX_OLD_GENERATION_SIZE_MB).toBe(1);
    expect(CODE_RANGE_SIZE_MB).toBe(2);
    expect(STACK_SIZE_MB).toBe(3);
    expect(TOTAL_RESOURCE_LIMIT_COUNT).toBe(4);
  });

  test("creates a Task instance and reads threadId", () => {
    const task = new Task(workerScriptPath, { name: "test-worker" });
    expect(typeof task.threadId).toBe("number");
    expect(task.threadId).toBeGreaterThan(0);
    expect(task.hasRef()).toBe(true);
    task.terminate();
  });

  test("getters and setters for onmessage, onerror, onmessageerror", () => {
    const task = new Task(workerScriptPath);
    expect(task.onmessage).toBeNull();
    expect(task.onerror).toBeNull();
    expect(task.onmessageerror).toBeNull();

    const handler = (e: any) => {};
    task.onmessage = handler;
    expect(task.onmessage).toBe(handler);

    const errHandler = (e: any) => {};
    task.onerror = errHandler;
    expect(task.onerror).toBe(errHandler);

    const msgErrHandler = (e: any) => {};
    task.onmessageerror = msgErrHandler;
    expect(task.onmessageerror).toBe(msgErrHandler);

    task.onmessage = null;
    expect(task.onmessage).toBeNull();

    task.terminate();
  });

  test("postMessage and onmessage communication with worker", async () => {
    const task = new Task(workerScriptPath);
    const received: string[] = [];

    await new Promise<void>((resolve) => {
      task.onmessage = (e: any) => {
        received.push(e.data);
        if (received.length === 2) resolve();
      };

      task.postMessage("ping");
      task.postMessage(["hello", "world"]);
    });

    expect(received).toEqual(["pong", "hello-world"]);
    task.terminate();
  });

  test("addEventListener and removeEventListener", async () => {
    const task = new Task(workerScriptPath);
    let count = 0;

    const listener = (e: any) => {
      if (e.data === "pong") {
        count++;
      }
    };

    task.addEventListener("message", listener);

    await new Promise<void>((resolve) => {
      task.onmessage = (e: any) => {
        if (e.data === "pong") resolve();
      };
      task.postMessage("ping");
    });

    expect(count).toBe(1);

    task.removeEventListener("message", listener);
    task.terminate();
  });

  test("dispatchEvent manually fires listeners", () => {
    const task = new Task(workerScriptPath);
    let dispatchedData: any = null;

    task.onmessage = (e: any) => {
      dispatchedData = e.data;
    };

    const handled = task.dispatchEvent({ type: "message", data: "manual-event" });
    expect(handled).toBe(true);
    expect(dispatchedData).toBe("manual-event");

    task.terminate();
  });

  test("ref, unref, and ancillary methods run without throwing", () => {
    const task = new Task(workerScriptPath);
    expect(() => task.unref()).not.toThrow();
    expect(() => task.ref()).not.toThrow();
    expect(() => task.startThread()).not.toThrow();
    expect(() => task.stopThread()).not.toThrow();
    expect(Array.isArray(task.getResourceLimits())).toBe(true);
    expect(task.loopIdleTime()).toBe(-1);
    expect(task.loopStartTime()).toBe(-1);
    task.terminate();
  });
});
