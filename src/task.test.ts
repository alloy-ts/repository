import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Task } from "./main.ts";

let tempDir: string;
let workerScript: string;

beforeAll(() => {
  tempDir = mkdtempSync(join(tmpdir(), "task-test-"));
  workerScript = join(tempDir, "worker.js");
  writeFileSync(
    workerScript,
    `
const { parentPort } = require('node:worker_threads');
parentPort.on('message', (msg) => {
  if (msg && msg.type === 'ping') {
    parentPort.postMessage({ type: 'pong', value: msg.value * 2 });
  }
});
`
  );
});

afterAll(() => {
  rmSync(tempDir, { recursive: true, force: true });
});

describe("Task (src/task.rs)", () => {
  test("creates a task and communicates via postMessage and onmessage", async () => {
    const task = new Task(workerScript);
    expect(typeof task.threadId).toBe("number");
    expect(task.threadId).toBeGreaterThan(0);

    const promise = new Promise<any>((resolve) => {
      task.onmessage = (e: any) => {
        resolve(e.data);
      };
    });

    task.postMessage({ type: "ping", value: 21 });
    const result = await promise;
    expect(result).toEqual({ type: "pong", value: 42 });

    await task.terminate();
  });

  test("addEventListener and dispatchEvent work on Task", async () => {
    const task = new Task(workerScript);

    const received: any[] = [];
    const listener = (e: any) => {
      received.push(e.data);
    };

    task.addEventListener("message", listener);
    task.postMessage({ type: "ping", value: 10 });

    await new Promise((r) => setTimeout(r, 100));
    expect(received.length).toBeGreaterThan(0);
    expect(received[0]).toEqual({ type: "pong", value: 20 });

    task.removeEventListener("message", listener);
    await task.terminate();
  });

  test("ref and unref do not throw", async () => {
    const task = new Task(workerScript);
    expect(() => task.unref()).not.toThrow();
    expect(() => task.ref()).not.toThrow();
    await task.terminate();
  });
});
