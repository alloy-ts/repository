import { describe, expect, test } from "vite-plus/test";
import { Task, TaskSignal } from "./main.ts";

// `src/task.rs` exports:
//
//   - `TaskSignal` — the libuv `uv_async_t` wakeup primitive: `send()` from any
//     thread schedules the constructor-given callback on the Node event loop
//     (sends are coalesced). This is how background tasks deliver progress and
//     results back to JavaScript.
//   - `Task` — a Executor-style task that resolves its `work` on a libuv
//     worker thread and evaluates it on the JS thread, emitting `message` /
//     `error` / `exit` events.
//
// NOTE: the native module under `dist/` must be rebuilt (`npm run build`) after
// the Rust bindings change for these tests to run.

describe("TaskSignal", () => {
  test("send() wakes the loop and invokes the callback", async () => {
    let resolveDone!: () => void;
    const done = new Promise<void>((resolve) => {
      resolveDone = resolve;
    });
    const signal = new TaskSignal(resolveDone);
    signal.send();
    await done;
    signal.close();
  });

  test("close() releases the handle without throwing", () => {
    const signal = new TaskSignal(() => {});
    expect(() => signal.close()).not.toThrow();
  });
});

describe("Task", () => {
  test("runs inline code, delivers postMessage and emits exit 0", async () => {
    const messages: unknown[] = [];
    const exited = new Promise<number>((resolve) => {
      const task = new Task({ work: "postMessage('hello uv');" });
      // Handlers receive the emitted event object: `{ type, data }`.
      task.onmessage = (evt: unknown) => {
        messages.push((evt as { data: unknown }).data);
      };
      task.addEventListener("exit", (evt: unknown) => {
        resolve((evt as { code: number }).code);
      });
    });
    expect(await exited).toBe(0);
    expect(messages).toContain("hello uv");
  });

  test("a throwing work emits exit code 1", async () => {
    const exited = new Promise<number>((resolve) => {
      const task = new Task({ work: "throw new Error('boom');" });
      task.addEventListener("exit", (evt: unknown) => {
        resolve((evt as { code: number }).code);
      });
    });
    expect(await exited).toBe(1);
  });

  test("exposes worker-like getters and taskData", async () => {
    const exited = new Promise<number>((resolve) => {
      const task = new Task({
        name: "probe",
        work: "",
        taskData: { answer: 42 },
      });
      expect(typeof task.threadId).toBe("number");
      expect(task.threadName).toBe("probe");
      expect(task.terminated).toBe(false);
      expect(task.hasRef).toBe(true);
      expect((task.taskData as { answer: number }).answer).toBe(42);
      task.addEventListener("exit", (evt: unknown) => {
        resolve((evt as { code: number }).code);
      });
    });
    expect(await exited).toBe(0);
  });

  test("terminate() resolves with an exit code and flags the task", async () => {
    const task = new Task({ work: "" });
    expect(await task.terminate()).toBe(1);
    expect(task.terminated).toBe(true);
  });
});
