import { describe, expect, test, afterAll, beforeAll } from "vitest";
import { Task } from "./main.ts";
import { writeFileSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

describe("Task (Threaded Task) API", () => {
  let taskScriptPath: string;

  beforeAll(() => {
    taskScriptPath = join(tmpdir(), `task_test_${Date.now()}.js`);
    writeFileSync(
      taskScriptPath,
      `
      const { parentPort } = require("node:worker_threads");
      if (parentPort) {
        parentPort.on("message", (msg) => {
          if (msg.type === "ping") {
            parentPort.postMessage({ type: "pong", payload: msg.payload });
          } else if (msg.type === "error") {
            throw new Error("Task thread error requested");
          }
        });
      }
      `
    );
  });

  afterAll(() => {
    try {
      unlinkSync(taskScriptPath);
    } catch {}
  });

  test("creates a task and reports valid threadId", async () => {
    const task = new Task(taskScriptPath, { name: "test-worker" });
    expect(typeof task.threadId).toBe("number");
    expect(task.threadId).toBeGreaterThan(0);
    expect(task.terminated).toBe(false);

    await task.terminate();
    expect(task.terminated).toBe(true);
  });

  test("sends and receives messages via postMessage and onmessage", async () => {
    const task = new Task(taskScriptPath);

    const messagePromise = new Promise<any>((resolve) => {
      task.onmessage = (e: any) => {
        resolve(e.data);
      };
    });

    task.postMessage({ type: "ping", payload: "hello world" });

    const response = await messagePromise;
    expect(response).toEqual({ type: "pong", payload: "hello world" });

    await task.terminate();
  });

  test("handles event listeners with addEventListener, removeEventListener, and dispatchEvent", async () => {
    const task = new Task(taskScriptPath);

    let customEventData: any = null;
    const listener = (e: any) => {
      customEventData = e.detail;
    };

    task.addEventListener("custom", listener);

    const dispatched = task.dispatchEvent({ type: "custom", detail: "dispatched-data" });
    expect(dispatched).toBe(true);
    expect(customEventData).toBe("dispatched-data");

    // Remove listener
    customEventData = null;
    task.removeEventListener("custom", listener);

    task.dispatchEvent({ type: "custom", detail: "new-data" });
    expect(customEventData).toBeNull();

    await task.terminate();
  });

  test("supports ref and unref methods", async () => {
    const task = new Task(taskScriptPath);

    expect(() => task.unref()).not.toThrow();
    expect(() => task.ref()).not.toThrow();

    await task.terminate();
  });

  test("handles onerror handler on worker error", async () => {
    const task = new Task(taskScriptPath);

    const errorPromise = new Promise<any>((resolve) => {
      task.onerror = (ev: any) => {
        resolve(ev);
      };
    });

    task.postMessage({ type: "error" });

    const errEvent = await errorPromise;
    expect(errEvent).toBeDefined();

    await task.terminate();
  });
});
