import { describe, expect, test } from "vite-plus/test";
import { CancellationToken, createCancellationToken, Process } from "./main.ts";

// `yes`/`sleep`/`echo`/`cat` are POSIX utilities; the tests below assume a
// POSIX shell is on PATH (macOS/Linux). The Windows equivalents are noted where
// relevant but the fixture set is intentionally POSIX-centric.
const longRunning = () =>
  process.platform === "win32"
    ? { program: "ping", args: ["-n", "2", "127.0.0.1"] }
    : { program: "sleep", args: ["5"] };

describe("Process", () => {
  test("runs a command and captures stdout", async () => {
    const proc = new Process({ program: "echo", args: ["hello", "world"] });
    const result = await proc.run();
    expect(result.success).toBe(true);
    expect(result.failure).toBeUndefined();
    const out = result.output!;
    expect(Buffer.isBuffer(out.stdout)).toBe(true);
    expect(out.stdout.toString().trim()).toBe("hello world");
  });

  test("treats a non-zero exit as a successful capture", async () => {
    const proc = new Process({ program: "sh", args: ["-c", "exit 7"] });
    const result = await proc.run();
    expect(result.success).toBe(true);
    // `status` shape comes from `crate::output::ProcessStatus`; we only assert
    // it is present and non-trivial here to stay independent of its fields.
    expect(result.output!.status).toBeTruthy();
  });

  test("writes stdin when input is provided", async () => {
    const proc = new Process({
      program: "cat",
      input: Buffer.from("piped input\n"),
    });
    const result = await proc.run();
    expect(result.success).toBe(true);
    expect(result.output!.stdout.toString()).toBe("piped input\n");
  });

  test("returns a structured failure on timeout", async () => {
    const proc = new Process({ ...longRunning(), timeoutMs: 200 });
    const result = await proc.run();
    expect(result.success).toBe(false);
    expect(result.output).toBeUndefined();
    expect(result.failure!.kind).toBe("timed out");
    expect(result.failure!.timeoutMs).toBeGreaterThan(0);
  });

  test("stops early when the cancellation token is signalled", async () => {
    const token = createCancellationToken();
    const proc = new Process(longRunning(), token);
    const promise = proc.run();
    token.cancel();
    const result = await promise;
    expect(result.success).toBe(false);
    expect(result.failure!.kind).toBe("cancelled");
  });

  test("honors per-stream output byte limits", async () => {
    // `yes` prints `y\n` forever; cap stdout at 16 bytes.
    if (process.platform === "win32") {
      return; // no `yes` on Windows
    }
    const proc = new Process({
      program: "yes",
      args: [],
      limits: { stdout: 16 },
      timeoutMs: 2000,
    });
    const result = await proc.run();
    expect(result.success).toBe(false);
    expect(result.failure!.kind).toBe("output limit exceeded");
    expect(result.failure!.stream).toBe("stdout");
    expect(result.failure!.limit).toBe(16);
    expect(result.failure!.capture.stdoutTruncated).toBe(true);
    // Only the retained prefix up to the limit is present.
    expect(result.failure!.stdout.length).toBeLessThanOrEqual(16);
  });

  test("exposes cleanup observations on a stopped execution", async () => {
    const proc = new Process({ ...longRunning(), timeoutMs: 200 });
    const result = await proc.run();
    expect(result.success).toBe(false);
    const cleanup = result.failure!.cleanup;
    expect(typeof cleanup.directChildReaped).toBe("boolean");
    expect(typeof cleanup.processTree).toBe("string");
    expect(typeof cleanup.waitTimedOut).toBe("boolean");
  });
});
