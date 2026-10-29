import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, test } from "vite-plus/test";
import {
  DistributedCoordinatorToTaskerMsg,
  DistributedTask,
  DistributedTaskConnectionPool,
  DistributedTaskUnitBatch,
  DistributedTaskUnitFeedContext,
  DistributedTaskUnitFeedProto,
  DistributedTaskUnitMsg,
  DistributedWorker,
  exec,
  Task,
  TaskSignal,
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

// ---------------------------------------------------------------------------
// DataFusion `Distributed*` napi bindings (src/task_napi.rs)
//
// These classes compile only with `--features datafusion`. On the default
// build they are not present in `dist/index.js`, so the suite is skipped.
// ---------------------------------------------------------------------------

const haveDatafusion = typeof DistributedTask !== "undefined";

(haveDatafusion ? describe : describe.skip)("DistributedTask (feature = datafusion)", () => {
  test("constructs and exposes a version string", () => {
    const t = new DistributedTask();
    expect(typeof t.version()).toBe("string");
  });

  test("from_default_session_builder yields a usable instance", () => {
    const t = DistributedTask.from_default_session_builder();
    expect(t).toBeInstanceOf(DistributedTask);
    expect(typeof t.version()).toBe("string");
  });

  test("builder chain threads version / max_message_size", () => {
    const t = new DistributedTask().with_version("1.2.3").with_max_message_size(1024);
    expect(t).toBeInstanceOf(DistributedTask);
    expect(t.version()).toBe("1.2.3");
  });

  test("DistributedTaskConnectionPool exposes metrics (opaque handle)", () => {
    const pool = new DistributedTaskConnectionPool(4);
    expect(pool).toBeInstanceOf(DistributedTaskConnectionPool);
    expect(() => pool.metrics()).not.toThrow();
  });

  test("DistributedTaskUnitFeedContext fan_out_tasks round-trips", () => {
    const ctx = new DistributedTaskUnitFeedContext(8);
    expect(ctx.fan_out_tasks()).toBe(8);
  });

  test("DistributedTaskUnitFeedProto bytes + id round-trip a UUID", () => {
    const id = "9f1c2d3e-4a5b-6c7d-8e9f-001122334455";
    const proto = DistributedTaskUnitFeedProto.from_id(id);
    expect(proto.id()).toBe(id);
    const bytes = proto.to_bytes();
    expect(bytes).toBeInstanceOf(Uint8Array);
    const back = DistributedTaskUnitFeedProto.from_bytes(bytes);
    expect(back.id()).toBe(id);
  });

  test("protobuf wrappers expose from_bytes / to_bytes", () => {
    for (const Ctor of [
      DistributedTaskUnitMsg,
      DistributedTaskUnitBatch,
      DistributedCoordinatorToTaskerMsg,
    ]) {
      expect(typeof Ctor).toBe("function");
      expect(typeof Ctor.prototype.from_bytes).toBe("function");
      expect(typeof Ctor.prototype.to_bytes).toBe("function");
    }
  });

  test("DistributedWorker is exposed as a class", () => {
    expect(typeof DistributedWorker).toBe("function");
  });
});
