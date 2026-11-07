# Process API

Source: [src/task/process.rs](../../../sources/task/src/process.rs).

`Process` launches a child executable and captures its stdout and stderr as buffers. Construction schedules execution immediately on the native async runtime; `run()` collects that execution once. The child may start and finish before `run()` is called.

This page describes the constructor changes in the draft branch. It does not describe Node.js's global `process` object. Host properties such as `pid`, `env`, and `argv`, signal events, and `EventEmitter` methods are not exposed by this class.

## Usage

The package currently exports these bindings from `repository`; separate `@alloy-ts/task/process` package exports are not configured.

```ts
import { Process } from "repository";

const child = new Process({
  program: process.execPath,
  args: ["-e", "console.log('hello from child')"],
  timeoutMs: 5000,
  limits: { stdout: 1024, stderr: 1024 },
});

const result = await child.run();
if (result.success) {
  console.log(result.output!.stdout.toString("utf8"));
  console.log("Exit code:", result.output!.exitCode);
} else {
  console.error(result.failure!.kind);
}
```

## Class: Process

### Constructor

```ts
new Process(input: string | URL | CommandOptions, cancellation?: CancellationToken)
```

| Input | Behavior |
| --- | --- |
| Executable string, such as `"git"` or `"/usr/bin/git"` | Launches that executable with no arguments. Bare names use normal OS executable lookup. |
| File URL string, such as `"file:///usr/bin/git"` | Converts the local file URL to an executable path, including percent decoding. |
| URL object, such as `new URL("file:///usr/bin/git")` | Reads its string `href` and resolves it as a local file URL. |
| Command descriptor, such as `{ program: "git", args: ["--version"] }` | Configures the command using the options below. An object's `program` property takes precedence over `href`. |

The object path uses property access rather than an `instanceof URL` check, so a URL-like object with a string `href` is also accepted. In a descriptor, `program` can itself be a local file URL string.

An executable string is not a shell command: `"git --version"` is treated as the executable name. Pass arguments through `args`. To execute a JavaScript file, use the Node executable as `program` and the script path as an argument.

HTTP(S) URLs are rejected; this constructor does not download executables. File URLs must convert to a local path and must not contain a query or fragment. Empty executable names and NUL bytes are rejected.

A “task object” here means a `CommandOptions` descriptor. A live [Task](./index.md) instance or a `TaskOptions` object with `entries` is not accepted.

Input validation can throw synchronously. OS spawn failures and execution errors are observed by awaiting `run()`. Construction schedules the work; it does not synchronously confirm that the OS has created the child.

### run(): Promise<CommandResult>

Collects the already-scheduled execution. It does not start a second child. A second call, including a concurrent call, rejects with `Process.run() may only be called once`.

Keep the instance reachable until its result has been collected. Dropping the native instance requests cancellation.

### cancel(): void

Requests cancellation through the process's token. Cancellation is sticky and repeated requests are harmless. This method does not wait for cleanup; await the original `run()` promise to inspect the result.

If the constructor received a shared token, `cancel()` also cancels other operations using that token. Otherwise the constructor allocates a token for this instance.

## CommandOptions

| Property | Type | Default / meaning |
| --- | --- | --- |
| `program` | `string` | Required executable name or path. The Process constructor also accepts a file URL string here. |
| `args` | `string[]` | No arguments. Passed directly to the executable. |
| `cwd` | `string` | Inherits the host working directory. |
| `env` | `{ name: string; value: string }[]` | Overrides entries in the inherited environment; does not replace the entire environment. |
| `input` | `Buffer` | Bytes written to piped stdin, followed by EOF. Without input, stdin is null. |
| `timeoutMs` | `number` | No execution timeout. Covers stdin writing, output collection, and child completion. |
| `cleanupTimeoutMs` | `number` | 5000 ms. Bounds cleanup waiting and pipe draining after an execution failure. |
| `limits` | `{ stdout?: number; stderr?: number }` | No capture caps. Each supplied value limits retained bytes for that stream. |

Timeout values must be finite, nonnegative, and representable as native durations. An unrepresentable deadline rejects before spawning. Reaching an output cap and then receiving additional bytes stops execution with a structured failure; it is not merely silent truncation. Output is collected in memory; no public stream handles are exposed.

## CommandResult

| Property | Type | Meaning |
| --- | --- | --- |
| `success` | `boolean` | Whether execution completed and output was captured, rather than interrupted. |
| `output` | `CommandOutput` or absent | Present when result `success` is true. |
| `failure` | `ExecutionFailure` or absent | Present when result `success` is false. |

A nonzero child exit code still produces `result.success === true` when capture completed normally. Check `result.output.success` or `exitCode` for the child command's success.

### CommandOutput

| Property | Type | Meaning |
| --- | --- | --- |
| `stdout`, `stderr` | `Buffer` | Captured raw output bytes. |
| `status` | `ProcessStatus` | Exact termination information. |
| `exitCode` | `number` | Exit code, or `-1` when unavailable. |
| `success` | `boolean` | Whether the child exited successfully. |

`ProcessStatus.kind` is `"exited"`, `"signaled"`, or `"unknown"`. Exited status includes `code`; signaled status includes `signal` and `coreDumped`.

### ExecutionFailure

`kind` is one of `"cancelled"`, `"timed out"`, `"stdin failure"`, `"output read failure"`, `"output limit exceeded"`, `"child wait failure"`, or `"process containment failure"`.

Failures include captured `stdout` and `stderr` buffers, optional `status`, and reason-specific optional fields: `message`, `stream`, `limit`, and `timeoutMs`.

The `cleanup` report includes `directChildReaped`, `directChildKillRequested`, optional `directChildWaitError`, `processTree`, and `waitTimedOut`. The `capture` report includes `stdoutPipeEof`, `stderrPipeEof`, `stdoutTruncated`, and `stderrTruncated`.

These reports record observed cleanup outcomes. An accepted termination request is not proof that every descendant exited. Descendants that escape process containment and abrupt host termination are outside the cleanup guarantees.

## CancellationToken

Create a valid token with `createCancellationToken()`. The low-level `CancellationToken(id)` constructor refers to an existing registry ID; it does not allocate fresh cancellation state.

| Method | Return | Meaning |
| --- | --- | --- |
| `cancel()` | `void` | Requests cancellation and wakes waiters. |
| `isCancelled()` | `boolean` | Reports whether cancellation was requested. |
| `cancelled()` | `Promise<void>` | Waits for cancellation. |

```ts
import { Process, createCancellationToken } from "repository";

const token = createCancellationToken();
const child = new Process({
  program: process.execPath,
  args: ["-e", "setInterval(() => {}, 1000)"],
}, token);

const completion = child.run();
token.cancel();
const result = await completion;
console.log(result.failure?.kind); // "cancelled"
```

Cancellation cannot be reset. A pre-cancelled token prevents the scheduled operation from spawning.

## runCommand(options, cancellation?): Promise<CommandResult>

Executes one command per call and returns the same result and failure shapes as `Process.run()`. This function takes `CommandOptions`, not the constructor's string/URL overloads. Use an executable name or filesystem path in its `program` field; URL normalization belongs to the Process constructor.

```ts
import { runCommand } from "repository";

const result = await runCommand({
  program: process.execPath,
  args: ["--version"],
});
console.log(result.output?.stdout.toString("utf8"));
```

There is no exported `Command` builder with `arg()`, `cwd()`, or `env()` methods; use the descriptor fields instead.
