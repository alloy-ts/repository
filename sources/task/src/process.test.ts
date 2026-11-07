import { access, mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { describe, expect, test } from "vite-plus/test";
import { createCancellationToken, Process, runCommand, Task } from "../../repository/src/main.ts";

// Use the running Node executable instead of platform-specific shell utilities.
// Bound fixture lifetimes even when the behavior under test fails.
const nodeCommand = (source: string, args: string[] = []) => ({
  program: process.execPath,
  args: ["-e", source, ...args],
  timeoutMs: 5000,
});
const longRunning = () => nodeCommand("setInterval(() => {}, 1000)");

async function withDirectory(run: (directory: string) => Promise<void>) {
  const directory = await mkdtemp(join(tmpdir(), "alloy-process-"));
  try {
    await run(directory);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
}

describe("Process constructor", () => {
  test("accepts an executable path string", async () => {
    // Without arguments Node reads stdin, which Process connects to null.
    const child = new Process(process.execPath);
    const result = await child.run();
    expect(result.success).toBe(true);
    expect(result.output!.exitCode).toBe(0);
  });

  test("accepts file URL strings and URL objects", async () => {
    const url = pathToFileURL(process.execPath);
    for (const input of [url.href, url]) {
      const child = new Process(input);
      const result = await child.run();
      expect(result.success).toBe(true);
      expect(result.output!.success).toBe(true);
    }
  });

  test("accepts a command descriptor with a file URL program", async () => {
    const child = new Process({
      ...nodeCommand("process.stdout.write(process.argv[1])", ["hello world"]),
      program: pathToFileURL(process.execPath).href,
    });
    const result = await child.run();
    expect(result.output!.stdout.toString("utf8")).toBe("hello world");
  });

  test("exposes normalized program and independent argument snapshots", async () => {
    const original = ["-e", "process.stdout.write(process.argv[1])", "original"];
    const child = new Process({
      program: pathToFileURL(process.execPath).href,
      args: original,
      timeoutMs: 5000,
    });
    original[2] = "mutated input";
    expect(child.program).toBe(process.execPath);
    const snapshot = child.args;
    snapshot[2] = "mutated snapshot";
    expect(child.args[2]).toBe("original");
    expect(child.id).toBeNull();
    const result = await child.run();
    expect(result.output!.stdout.toString("utf8")).toBe("original");
    await expect(child.run()).rejects.toThrow("only be called once");
  });

  test("starts before run and does not execute again", async () => {
    await withDirectory(async (directory) => {
      const marker = join(directory, "started");
      const child = new Process(
        nodeCommand("require('node:fs').appendFileSync(process.argv[1], 'started\\n')", [marker]),
      );
      let completion: ReturnType<Process["run"]> | undefined;
      try {
        await expect
          .poll(
            async () => {
              try {
                await access(marker);
                return true;
              } catch {
                return false;
              }
            },
            { timeout: 4000 },
          )
          .toBe(true);

        completion = child.run();
        expect((await completion).success).toBe(true);
        await expect(child.run()).rejects.toThrow("only be called once");
        expect(await readFile(marker, "utf8")).toBe("started\n");
      } finally {
        child.cancel();
        // Collect cleanup even if the startup assertion failed.
        await (completion ?? child.run());
      }
    });
  });

  test("allows only one concurrent result collector", async () => {
    const child = new Process(nodeCommand("setTimeout(() => {}, 100)"));
    const results = await Promise.allSettled([child.run(), child.run()]);
    const fulfilled = results.filter((result) => result.status === "fulfilled");
    const rejected = results.filter((result) => result.status === "rejected");
    expect(fulfilled).toHaveLength(1);
    expect(rejected).toHaveLength(1);
    if (rejected[0].status === "rejected") {
      expect(String(rejected[0].reason)).toContain("only be called once");
    }
  });

  test("rejects empty, NUL-containing, remote and decorated URL inputs", () => {
    const executable = pathToFileURL(process.execPath).href;
    const inputs = [
      "",
      "bad\0path",
      "file:///tmp/bad%00path",
      "https://example.invalid/program",
      new URL("https://example.invalid/program"),
      executable + "?argument=1",
      executable + "#fragment",
    ];
    for (const input of inputs) {
      expect(() => new Process(input)).toThrow();
    }
  });

  test("runs TaskOptions in a separate process with taskData and ordered entries", async () => {
    const parentPid = process.pid;
    const child = new Process({
      entries: [
        "process.stdout.write(JSON.stringify({ pid: process.pid, data: taskData, env: process.env.ALLOY_TASK_VALUE, arg: process.argv[1] }))",
        "process.stderr.write('second entry')",
      ],
      taskData: { message: "hello" },
      env: { ALLOY_TASK_VALUE: "child" },
      argv: ["argument"],
    });
    const result = await child.run();
    expect(result.output!.success).toBe(true);
    const output = JSON.parse(result.output!.stdout.toString());
    expect(output.pid).not.toBe(parentPid);
    expect(output.data).toEqual({ message: "hello" });
    expect(output.env).toBe("child");
    expect(output.arg).toBe("argument");
    expect(result.output!.stderr.toString()).toBe("second entry");
    expect(process.env.ALLOY_TASK_VALUE).toBeUndefined();
  });

  test("replays an existing Task's original options in a fresh child", async () => {
    const data = { pid: process.pid, message: "original" };
    const task = new Task({
      entries: "if (process.pid !== taskData.pid) process.stdout.write(taskData.message)",
      taskData: data,
    });
    data.message = "mutated";
    await task.done();
    const child = new Process(task);
    const result = await child.run();
    expect(result.output!.success).toBe(true);
    expect(result.output!.stdout.toString()).toBe("original");
  });

  test("resolves TaskOptions file entries and reports task failures", async () => {
    const child = new Process({ entries: "throw new Error('task failed')" });
    const result = await child.run();
    expect(result.output!.success).toBe(false);
    expect(result.output!.exitCode).toBe(1);
    expect(result.output!.stderr.toString()).toContain("task failed");
    await withDirectory(async (directory) => {
      const { writeFile } = await import("node:fs/promises");
      const script = join(directory, "entry.cjs");
      await writeFile(script, "process.stdout.write('file entry')");
      const fileChild = new Process({ entries: pathToFileURL(script).href });
      expect((await fileChild.run()).output!.stdout.toString()).toBe("file entry");
    });
  });

  test("cancels a task subprocess without stopping its source Task", async () => {
    const task = new Task({ entries: "setTimeout(() => {}, 10)" });
    await task.done();
    const child = new Process(task);
    child.cancel();
    const result = await child.run();
    expect(result.failure!.kind).toBe("cancelled");
    expect(task.terminated).toBe(false);
  });

  test("applies an empty environment only inside the task subprocess", async () => {
    const child = new Process({
      entries: "process.stdout.write(String(Object.keys(process.env).length))",
      env: "none",
    });
    const result = await child.run();
    expect(result.output!.success).toBe(true);
    expect(result.output!.stdout.toString()).toBe("0");
    expect(Object.keys(process.env).length).toBeGreaterThan(0);
  });

  test("transports task data larger than the OS command argument limit", async () => {
    const child = new Process({
      entries: "process.stdout.write(String(taskData.length))",
      taskData: "x".repeat(256 * 1024),
    });
    const result = await child.run();
    expect(result.output!.success).toBe(true);
    expect(result.output!.stdout.toString()).toBe(String(256 * 1024));
    expect(child.args.join(" ")).not.toContain("x".repeat(100));
  });

  test("rejects malformed task options and unrelated native classes", () => {
    for (const invalid of [{ entries: 12 }, { entries: [true] }, createCancellationToken()]) {
      expect(() => new Process(invalid as never)).toThrow();
    }
  });

  test("rejects invalid execution and cleanup durations synchronously", () => {
    for (const value of [NaN, Infinity, -Infinity, -1, Number.MAX_VALUE]) {
      expect(
        () =>
          new Process({
            ...nodeCommand(""),
            timeoutMs: value,
          }),
      ).toThrow();
      expect(
        () =>
          new Process({
            ...nodeCommand(""),
            cleanupTimeoutMs: value,
          }),
      ).toThrow();
    }
  });

  test("reports missing executables through run rejection", async () => {
    await withDirectory(async (directory) => {
      const child = new Process({ program: join(directory, "missing-executable") });
      await expect(child.run()).rejects.toThrow();
    });
  });
});

describe("Process capture and options", () => {
  test("captures stdout and stderr separately as buffers", async () => {
    const child = new Process(
      nodeCommand("process.stdout.write('hello'); process.stderr.write('diagnostic')"),
    );
    const result = await child.run();
    expect(result.success).toBe(true);
    expect(result.failure).toBeUndefined();
    const output = result.output!;
    expect(Buffer.isBuffer(output.stdout)).toBe(true);
    expect(Buffer.isBuffer(output.stderr)).toBe(true);
    expect(output.stdout.toString("utf8")).toBe("hello");
    expect(output.stderr.toString("utf8")).toBe("diagnostic");
    expect(output.status.kind).toBe("exited");
    expect(output.status.code).toBe(0);
    expect(output.exitCode).toBe(0);
    expect(output.success).toBe(true);
  });

  test("distinguishes completed capture from a successful child exit", async () => {
    const child = new Process(nodeCommand("process.exitCode = 7"));
    const result = await child.run();
    expect(result.success).toBe(true);
    expect(result.failure).toBeUndefined();
    expect(result.output!.success).toBe(false);
    expect(result.output!.exitCode).toBe(7);
    expect(result.output!.status.kind).toBe("exited");
    expect(result.output!.status.code).toBe(7);
  });

  test("preserves binary stdin and closes the input pipe", async () => {
    const input = Buffer.from([0, 1, 127, 128, 255, 10]);
    const child = new Process({
      ...nodeCommand("process.stdin.pipe(process.stdout)"),
      input,
    });
    const result = await child.run();
    expect(result.success).toBe(true);
    expect(result.output!.stdout).toEqual(input);
  });

  test("applies cwd and env overrides without changing the host", async () => {
    await withDirectory(async (directory) => {
      const originalCwd = process.cwd();
      const originalValue = process.env.ALLOY_PROCESS_TEST_VALUE;
      const child = new Process({
        ...nodeCommand(
          "process.stdout.write(JSON.stringify({ cwd: process.cwd(), value: process.env.ALLOY_PROCESS_TEST_VALUE }))",
        ),
        cwd: directory,
        env: [{ name: "ALLOY_PROCESS_TEST_VALUE", value: "child-only" }],
      });
      const result = await child.run();
      expect(result.success).toBe(true);
      const output = JSON.parse(result.output!.stdout.toString("utf8"));
      // realpath accounts for aliases such as /var -> /private/var on macOS.
      const { realpath } = await import("node:fs/promises");
      expect(await realpath(output.cwd)).toBe(await realpath(directory));
      expect(output.value).toBe("child-only");
      expect(process.cwd()).toBe(originalCwd);
      expect(process.env.ALLOY_PROCESS_TEST_VALUE).toBe(originalValue);
    });
  });

  for (const stream of ["stdout", "stderr"] as const) {
    test("enforces the " + stream + " byte limit", async () => {
      const child = new Process({
        ...nodeCommand("process." + stream + ".write('abcdef')"),
        limits: { [stream]: 3 },
      });
      const result = await child.run();
      expect(result.success).toBe(false);
      expect(result.output).toBeUndefined();
      const failure = result.failure!;
      expect(failure.kind).toBe("output limit exceeded");
      expect(failure.stream).toBe(stream);
      expect(failure.limit).toBe(3);
      expect(failure[stream].toString("utf8")).toBe("abc");
      expect(
        stream === "stdout" ? failure.capture.stdoutTruncated : failure.capture.stderrTruncated,
      ).toBe(true);
    });
  }

  test("does not treat an exact output cap as an overflow", async () => {
    const child = new Process({
      ...nodeCommand("process.stdout.write('abc')"),
      limits: { stdout: 3 },
    });
    const result = await child.run();
    expect(result.success).toBe(true);
    expect(result.output!.stdout.toString("utf8")).toBe("abc");
  });
});

describe("Process cancellation and timeout", () => {
  test("returns structured timeout and cleanup observations", async () => {
    const child = new Process({
      ...longRunning(),
      timeoutMs: 100,
      cleanupTimeoutMs: 1000,
    });
    const result = await child.run();
    expect(result.success).toBe(false);
    expect(result.output).toBeUndefined();
    const failure = result.failure!;
    expect(failure.kind).toBe("timed out");
    expect(failure.timeoutMs).toBe(100);
    expect(Buffer.isBuffer(failure.stdout)).toBe(true);
    expect(Buffer.isBuffer(failure.stderr)).toBe(true);
    expect(typeof failure.cleanup.directChildReaped).toBe("boolean");
    expect(typeof failure.cleanup.directChildKillRequested).toBe("boolean");
    expect(typeof failure.cleanup.processTree).toBe("string");
    expect(typeof failure.cleanup.waitTimedOut).toBe("boolean");
    expect(typeof failure.capture.stdoutPipeEof).toBe("boolean");
    expect(typeof failure.capture.stderrPipeEof).toBe("boolean");
  });

  test("cancels through Process.cancel without an explicit token", async () => {
    const child = new Process(longRunning());
    const completion = child.run();
    child.cancel();
    child.cancel();
    const result = await completion;
    expect(result.success).toBe(false);
    expect(result.failure!.kind).toBe("cancelled");
  });

  test("cancels all processes sharing a token", async () => {
    const token = createCancellationToken();
    const first = new Process(longRunning(), token);
    const second = new Process(longRunning(), token);
    const completions = [first.run(), second.run()];
    first.cancel();
    const results = await Promise.all(completions);
    expect(token.isCancelled()).toBe(true);
    for (const result of results) {
      expect(result.success).toBe(false);
      expect(result.failure!.kind).toBe("cancelled");
    }
  });

  test("does not spawn when the token was already cancelled", async () => {
    await withDirectory(async (directory) => {
      const marker = join(directory, "must-not-exist");
      const token = createCancellationToken();
      token.cancel();
      const child = new Process(
        nodeCommand("require('node:fs').writeFileSync(process.argv[1], 'unexpected')", [marker]),
        token,
      );
      const result = await child.run();
      expect(result.success).toBe(false);
      expect(result.failure!.kind).toBe("cancelled");
      await expect(access(marker)).rejects.toThrow();
    });
  });

  test("tokens stay cancelled and notify current and later waiters", async () => {
    const token = createCancellationToken();
    expect(token.isCancelled()).toBe(false);
    const firstWaiter = token.cancelled();
    token.cancel();
    token.cancel();
    await firstWaiter;
    await token.cancelled();
    expect(token.isCancelled()).toBe(true);
  });
});

describe("runCommand", () => {
  test("executes independently on each call", async () => {
    for (let index = 0; index < 2; index++) {
      const result = await runCommand(
        nodeCommand("process.stdout.write(process.argv[1])", [String(index)]),
      );
      expect(result.success).toBe(true);
      expect(result.output!.stdout.toString("utf8")).toBe(String(index));
    }
  });
});
