import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, test } from "vite-plus/test";
import { Repository, Task, TaskExecutor } from "./main.ts";

// `Task` is a libuv `uv_queue_work` task that presents a Node-Worker-compatible
// API (`threadId`, `postMessage`, `terminate`, `reference`/`unreference`,
// `onmessage`, `addEventListener`, ...). It is NOT a wrapper around a Node worker
// thread — the Worker-shaped surface is there for compatibility, not because a
// worker is involved.
//
// JavaScript cannot execute on a libuv thread-pool thread, so the work is split:
// blocking setup (sandbox worktree creation, object-database reads, network
// fetches) runs on the pool, and the `programme` itself is evaluated on the JS
// thread in the after-work callback.
//
// There is no `globalThis` bootstrap to install: the public napi `Task` class
// holds its own state, and JS event handlers are kept alive natively.
//
// A programme is wrapped in `(function (postMessage, onMessage, taskData) {...})`,
// so it gets a worker-like messaging surface without a worker.
//
// NOTE: the native module under `dist/` must be rebuilt (`napi build`) after the
// `Task` Rust bindings were added for these tests to run.

/// Resolve with the task's exit code the first time it exits.
function onceExit(task: Task): Promise<number> {
  return new Promise<number>((resolve) => {
    task.addEventListener("exit", (event: { code?: number }) => resolve(event.code ?? 0));
  });
}

/// Resolve with the first message the task posts back.
function onceMessage(task: Task): Promise<unknown> {
  return new Promise<unknown>((resolve) => {
    task.addEventListener("message", (event: { data?: unknown }) => resolve(event.data));
  });
}

/// Write `source` to a fresh `.cjs` file and return its path.
function scriptFile(source: string): string {
  const dir = mkdtempSync(join(tmpdir(), "task-script-"));
  const file = join(dir, "programme.cjs");
  writeFileSync(file, source);
  return file;
}

describe("Task construction", () => {
  test("constructs from an inline programme and reports a thread id", async () => {
    const task = new Task({ programme: "console.log(64+3.13333333)" });
    expect(typeof task.threadId).toBe("number");
    expect(task.threadId).toBeGreaterThan(0);
    expect(await onceExit(task)).toBe(0);
  });

  test("evaluates an inline programme and posts its value back", async () => {
    const task = new Task({ programme: "postMessage(64+3.13333333)" });
    expect(await onceMessage(task)).toBeCloseTo(67.13333333, 8);
    expect(await onceExit(task)).toBe(0);
  });

  test("runs a programme given by file path", async () => {
    const task = new Task({
      programme: scriptFile("postMessage('from-file');"),
    });
    expect(await onceMessage(task)).toBe("from-file");
    expect(await onceExit(task)).toBe(0);
  });

  test("runs a programme stored as a git blob, addressed by object id", async () => {
    const repo = new Repository();
    const id = repo.writeBlob(Buffer.from("postMessage('from-blob');"));
    expect(id).toMatch(/^[0-9a-f]{40}$/);

    const task = new Task({ programme: id });
    expect(await onceMessage(task)).toBe("from-blob");
    expect(await onceExit(task)).toBe(0);
  });

  test("an unknown object id falls through and is run as inline code", async () => {
    // 40 hex chars, so it parses as an object id, but nothing is stored there.
    const missing = "0".repeat(40);
    // Evaluated as code this is the number 0, which is a valid (no-op) programme.
    const task = new Task({ programme: missing });
    expect(await onceExit(task)).toBe(0);
  });

  test("a programme that throws surfaces an error event", async () => {
    const task = new Task({ programme: "throw new Error('boom');" });
    const errors: unknown[] = [];
    task.addEventListener("error", (event: unknown) => errors.push(event));
    expect(await onceExit(task)).toBe(1);
    expect(errors).toHaveLength(1);
  });

  // Drafted but skipped: `build.ts` is TypeScript, and it cannot be loaded
  // without type-stripping support. Running it would also trigger a real
  // `napi build` as a side effect. Enable once the runtime supports it.
  test.skip("runs a programme given by path (build.ts)", async () => {
    const task = new Task({ programme: "build.ts" });
    expect(await onceExit(task)).toBe(0);
  });
});

describe("Task options", () => {
  test("sandbox creates a worktree under the OS temp directory", async () => {
    const repo = new Repository();
    const before = repo.worktrees().map((w) => w.id);

    const task = new Task({
      programme: "postMessage('sandboxed');",
      sandbox: true,
    });
    expect(await onceMessage(task)).toBe("sandboxed");
    expect(await onceExit(task)).toBe(0);

    const added = repo.worktrees().filter((w) => !before.includes(w.id));
    expect(added).toHaveLength(1);
    // The checkout location is the temp directory, not the working directory.
    expect(added[0].base).toContain(tmpdir());
  });

  test("each sandboxed task gets its own worktree", async () => {
    const repo = new Repository();
    const before = repo.worktrees().map((w) => w.id);

    const a = new Task({ programme: "postMessage('a');", sandbox: true });
    const b = new Task({ programme: "postMessage('b');", sandbox: true });
    await onceExit(a);
    await onceExit(b);

    const added = repo.worktrees().filter((w) => !before.includes(w.id));
    expect(added).toHaveLength(2);
    expect(new Set(added.map((w) => w.base)).size).toBe(2);
  });

  test('worktreePath "." runs against the real working directory', async () => {
    const repo = new Repository();
    const before = repo.worktrees().length;

    const task = new Task({
      programme: "postMessage(process.cwd());",
      worktreePath: ".",
    });
    expect(await onceMessage(task)).toBe(process.cwd());
    expect(await onceExit(task)).toBe(0);

    // No temp worktree was created for it.
    expect(repo.worktrees()).toHaveLength(before);
  });

  test('worktreePath "." overrides sandbox', async () => {
    const repo = new Repository();
    const before = repo.worktrees().length;

    const task = new Task({
      programme: "postMessage(process.cwd());",
      sandbox: true,
      worktreePath: ".",
    });
    expect(await onceMessage(task)).toBe(process.cwd());
    expect(await onceExit(task)).toBe(0);
    expect(repo.worktrees()).toHaveLength(before);
  });

  test("a network programme is refused without allowNetwork", () => {
    expect(() => new Task({ programme: "https://example.invalid/probe.js" })).toThrow(
      /allowNetwork/,
    );
  });

  test("name and resource limits are surfaced through getters", async () => {
    const task = new Task({
      programme: "console.log('getters')",
      name: "probe",
      resourceLimits: { maxOldGenerationSizeMb: 64 },
    });
    expect(task.threadName).toBe("probe");
    expect(task.resourceLimits?.maxOldGenerationSizeMb).toBe(64);
    expect(task.terminated).toBe(false);
    expect(task.hasRef).toBe(true);
    await onceExit(task);
  });
});

describe("Task lifecycle", () => {
  test("postMessage round-trips to the task and back", async () => {
    const task = new Task({
      programme: "onMessage((m) => postMessage(m * 2));",
    });
    await onceExit(task);
    task.postMessage(21);
    expect(await onceMessage(task)).toBe(42);
  });

  test("onmessage receives what the task posts", async () => {
    const task = new Task({ programme: "postMessage('hello');" });
    const seen: unknown[] = [];
    task.onmessage = (event: { data?: unknown }) => {
      seen.push(event.data);
    };
    await onceExit(task);
    expect(seen).toEqual(["hello"]);
  });

  test("terminate() marks the task terminated", async () => {
    const task = new Task({ programme: "console.log('done');" });
    await onceExit(task);
    const code = await task.terminate();
    expect(task.terminated).toBe(true);
    expect(typeof code).toBe("number");
  });

  test("unreference() and reference() flip hasRef", async () => {
    const task = new Task({ programme: "console.log('ref')" });
    task.unreference();
    expect(task.hasRef).toBe(false);
    task.reference();
    expect(task.hasRef).toBe(true);
    await onceExit(task);
  });
});

// `TaskExecutor` is the libuv `uv_queue_work` binding: the work callback runs on
// the libuv thread pool and completion is reported back to the JS (V8) thread.
// `Task` is built on the same primitive; this block pins the primitive itself.
describe("libuv work (TaskExecutor)", () => {
  const fib = (n: number): number => {
    let a = 0;
    let b = 1;
    for (let i = 0; i < n; i++) {
      const c = a + b;
      a = b;
      b = c;
    }
    return b;
  };

  test("submit() resolves with the value computed on the libuv pool", async () => {
    const exec = new TaskExecutor();
    const req = exec.submit(10);
    expect(req.type_name()).toBe("WORK");
    expect(await req.promise()).toBe(fib(11));
  });

  test("progress is reported back on the JS thread", async () => {
    const exec = new TaskExecutor();
    const seen: number[] = [];
    const req = exec.submit(10, (pct: number) => seen.push(pct));
    await req.promise();
    expect(seen.length).toBeGreaterThan(0);
    expect(seen[seen.length - 1]).toBe(100);
  });
});
