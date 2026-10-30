import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, test } from "vite-plus/test";
import {
  availableParallelism,
  blockOnTask,
  getEnvMessagePort,
  isMainThread,
  K_CODE_RANGE_SIZE_MB,
  K_MAX_OLD_GENERATION_SIZE_MB,
  K_MAX_YOUNG_GENERATION_SIZE_MB,
  K_MB,
  K_STACK_SIZE_MB,
  K_TOTAL_RESOURCE_LIMIT_COUNT,
  run,
  spawnLocalTask,
  Task,
  TaskSignal,
  Worker,
} from "./main.ts";

// `src/task.rs` exports:
//
//   - `TaskSignal` — the libuv `uv_async_t` wakeup primitive: `send()` from any
//     thread schedules the constructor-given callback on the Node event loop
//     (sends are coalesced). This is how background tasks deliver progress and
//     results back to JavaScript.
//   - `Task` — a Worker-style task (mirroring Node's internal
//     `internalBinding('task').Task`) that resolves its `entries` on a libuv
//     worker thread and evaluates them in order on the JS thread, emitting
//     `message` / `error` / `exit` events. It is promise-based: `done()`
//     resolves once the work has run, and `terminate()` resolves with the exit
//     code once `exit` has been emitted.
//   - `run()` / `TaskRequest` — the raw `uv_queue_work` + `uv_cancel`
//     thread-pool work scheduling surface.
//
// `TaskOptions.entries` accepts a single entry (inline code, a git blob id, a
// file path/URL, an http(s) URL) or an ordered list of entries, which run in
// one scope sharing `postMessage` / `onMessage` / `taskData`.
//
// NOTE: the native module under `dist/` must be rebuilt (`npm run build`) after
// the Rust bindings change for these tests to run.

/// Resolve with the task's exit code the first time it exits.
function onceExit(task: Task): Promise<number> {
  return new Promise<number>((resolve) => {
    task.addEventListener("exit", (evt: unknown) => resolve((evt as { code: number }).code));
  });
}

/// Resolve with the first message the task posts back.
function onceMessage(task: Task): Promise<unknown> {
  return new Promise<unknown>((resolve) => {
    task.addEventListener("message", (evt: unknown) => resolve((evt as { data: unknown }).data));
  });
}

/// Write `source` to a fresh `.cjs` file and return its path.
function scriptFile(source: string): string {
  const dir = mkdtempSync(join(tmpdir(), "task-script-"));
  const file = join(dir, "entry.cjs");
  writeFileSync(file, source);
  return file;
}

describe("Task constants and helper functions", () => {
  test("resource limit constants are defined", () => {
    expect(K_MB).toBe(1024 * 1024);
    expect(K_MAX_YOUNG_GENERATION_SIZE_MB).toBe(0);
    expect(K_MAX_OLD_GENERATION_SIZE_MB).toBe(1);
    expect(K_CODE_RANGE_SIZE_MB).toBe(2);
    expect(K_STACK_SIZE_MB).toBe(3);
    expect(K_TOTAL_RESOURCE_LIMIT_COUNT).toBe(4);
  });

  test("helper functions return expected values", () => {
    expect(isMainThread()).toBe(true);
    expect(getEnvMessagePort()).toBeNull();
    expect(typeof availableParallelism()).toBe("number");
    expect(availableParallelism()).toBeGreaterThan(0);
  });

  test("spawnLocalTask and blockOnTask helper functions", async () => {
    const task = spawnLocalTask({ entries: "postMessage('local-task');" });
    const msgPromise = onceMessage(task);
    await blockOnTask(task);
    expect(await msgPromise).toBe("local-task");
  });
});

describe("Worker class", () => {
  test("runs inline code via Worker alias", async () => {
    const worker = new Worker({ entries: "postMessage('from-worker');" });
    expect(await onceMessage(worker as unknown as Task)).toBe("from-worker");
    expect(await onceExit(worker as unknown as Task)).toBe(0);
  });
});

describe("TaskSignal (uv_async_t)", () => {
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
      const task = new Task({ entries: "postMessage('hello uv');" });
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

  test("runs a list of entries in order in one scope", async () => {
    const messages: unknown[] = [];
    const exited = new Promise<number>((resolve) => {
      const task = new Task({
        entries: [
          "globalThis.__seq = 0; postMessage('first');",
          "globalThis.__seq += 1; postMessage(globalThis.__seq);",
          "postMessage('last');",
        ],
      });
      task.onmessage = (evt: unknown) => {
        messages.push((evt as { data: unknown }).data);
      };
      task.addEventListener("exit", (evt: unknown) => {
        resolve((evt as { code: number }).code);
      });
    });
    expect(await exited).toBe(0);
    expect(messages).toEqual(["first", 1, "last"]);
  });

  test("a failing entry stops the sequence with exit code 1", async () => {
    const messages: unknown[] = [];
    const exited = new Promise<number>((resolve) => {
      const task = new Task({
        entries: ["postMessage('ok');", "throw new Error('stop');", "postMessage('never');"],
      });
      task.onmessage = (evt: unknown) => {
        messages.push((evt as { data: unknown }).data);
      };
      task.addEventListener("exit", (evt: unknown) => {
        resolve((evt as { code: number }).code);
      });
    });
    expect(await exited).toBe(1);
    expect(messages).toEqual(["ok"]);
  });

  test("runs an entry given by file path", async () => {
    const task = new Task({ entries: scriptFile("postMessage('from-file');") });
    expect(await onceMessage(task)).toBe("from-file");
    expect(await onceExit(task)).toBe(0);
  });

  test("a throwing entry emits exit code 1", async () => {
    const exited = new Promise<number>((resolve) => {
      const task = new Task({ entries: "throw new Error('boom');" });
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
        entries: "",
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

  test("done() resolves once the work has run", async () => {
    const task = new Task({ entries: "postMessage('done-marker');" });
    await expect(task.done()).resolves.toBeUndefined();
    expect(task.terminated).toBe(false);
  });

  test("terminate() before evaluation resolves with exit code 1", async () => {
    const task = new Task({ entries: "postMessage('never-runs');" });
    const code = await task.terminate();
    expect(code).toBe(1);
    expect(task.terminated).toBe(true);
  });

  test("terminate() after completion resolves with the recorded exit code", async () => {
    const task = new Task({ entries: "" });
    expect(await onceExit(task)).toBe(0);
    expect(await task.terminate()).toBe(0);
  });

  test("cancel() issues uv_cancel on the underlying request", () => {
    const task = new Task({ entries: "" });
    // True if the work had not started yet (the after-work callback then runs
    // with UV_ECANCELED and `error` + `exit 1` are emitted), false otherwise.
    // Either way the call is safe.
    expect(typeof task.cancel()).toBe("boolean");
  });
});

describe("exec (uv_queue_work / uv_cancel)", () => {
  test("runs work on the pool and after on the loop, in order", async () => {
    const seen: string[] = [];
    await new Promise<void>((resolve) => {
      const req = run(
        () => seen.push("work"),
        () => {
          seen.push("after");
          resolve();
        },
      );
      expect(req.typeName()).toBe("WORK");
    });
    expect(seen).toEqual(["work", "after"]);
  });

  test("cancel() is callable and reports whether it took effect", () => {
    const req = run(
      () => {},
      () => {},
    );
    expect(typeof req.cancel()).toBe("boolean");
  });
});
