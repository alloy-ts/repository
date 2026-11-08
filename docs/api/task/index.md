# Task API

The `Task` API queues entry resolution through Node-API async work and evaluates the resolved entries on the JavaScript event-loop thread in the current NAPI environment. For a separate operating-system process, use [Process](./process.md).

## Overview

A `Task` queues work when constructed. Blocking entry resolution runs on a libuv pool thread; JavaScript evaluation runs in the after-work callback on the event-loop thread. Entries run in order and share the runner's `postMessage`, `onMessage`, and `taskData` bindings. The first resolution or evaluation failure stops the sequence.

A Task is not an isolated Node.js instance, OS thread, or child process. CPU-bound JavaScript can block the host event loop. Entries are evaluated by the host runtime; Task does not provide a separate TypeScript transpiler.

| API | Execution | Completion | Cancellation |
| --- | --- | --- | --- |
| `Task` | Resolves entries in a pool, evaluates JavaScript in the current environment | `done()` | `cancel()` for queued work; `terminate()` requests skipping evaluation |
| `Process` | Starts a separate executable on construction | `run()` collects once | `cancel()` requests child cleanup |

Process accepts a live Task or TaskOptions with `entries` and starts a fresh subprocess execution from the original options. It also accepts executable paths, local file URLs, and command descriptors using `program` and `args`. Running state and listeners remain with the original Task.

Submodules under `task`:

- **`cluster`** ([cluster.md](./cluster.md)): Draft subprocess registry with stable task IDs and targeted cancellation.
- **`thread`** ([thread.md](./thread.md)): Direct OS thread handles (`Thread`, `Builder`, `JoinHandle`) and thread-pool wrappers (`ThreadPool`).
- **`scheduler`** ([scheduler.md](./scheduler.md)): Task scheduling and dispatching (`Scheduler`, `TaskHandle`) managing worker execution pools.
- **`promise`** ([promise.md](./promise.md)): Native Promise bridging and deferred resolution (`asyncPlus100`, `processBuffer`, `processBufferOnPool`).
- **`process`** ([process.md](./process.md)): Child process construction, buffered stdio capture, cancellation tokens, and timeouts.

## Usage

```ts
import { Task } from "repository";

const task = new Task({
  entries: "postMessage('hello from task');",
  name: "worker-task",
  taskData: { key: "value" },
});

task.onmessage = (event) => {
  console.log("Message from task:", event.data);
};

await task.done();
```

## API Reference

### `Task` Class

#### Constructor

```ts
new Task(options: TaskOptions)
```

Queues the configured entries immediately. Install listeners after construction and use `done()` to observe completion.

#### TaskOptions

| Property | Type | Meaning |
| --- | --- | --- |
| `entries` | `string \| string[]` | Required inline code, script path, file URL, or repository blob ID. Arrays execute in order. |
| `name` | `string` | Optional task name. |
| `taskData` | JSON-compatible value | Data exposed as `taskData` in the runner scope. |
| `tasktreePath` | `string` | Tasktree ID or path; `"."` explicitly selects the real working directory. |
| `sandbox` | `boolean` | Requests a separate tasktree for file resolution. This is filesystem separation, not process or V8 isolation. |
| `allowNetwork` | `boolean` | Allows HTTP(S) entry fetching; disabled by default. Fetching requires `curl`. |
| `repository` | `Repository` | Repository used to resolve Git blob IDs; otherwise discovered from the environment. |
| `resourceLimits` | `TaskResourceLimits` | Recorded resource settings; does not create a separate V8 isolate with enforced limits. |
| `env` | `"inherit" \| "none" \| object` | Recorded only; does not apply per-task environment overrides. |
| `argv`, `execArgv` | `string[]` | Recorded options; do not change the host's command-line arguments. |
| `trackUnmanagedFds` | `boolean` | Accepted option; does not establish file-descriptor isolation. |

`TaskResourceLimits` accepts optional `maxYoungGenerationSizeMb`, `maxOldGenerationSizeMb`, `codeRangeSizeMb`, and `stackSizeMb` numbers.

Task entry URLs and executable URLs have different behavior: Task may fetch HTTP(S) entries when `allowNetwork` is true; Process accepts local executable file URLs and does not download programs.

#### Properties

| Property         | Type                         | Description                                          |
| ---------------- | ---------------------------- | ---------------------------------------------------- |
| `threadId`       | `number`                     | Unique integer thread/task identifier.               |
| `threadName`     | `string`                     | The task's assigned name.                            |
| `hasRef`         | `boolean`                    | Recorded reference flag (true by default). |
| `terminated`     | `boolean`                    | Whether `terminate()` has been called.               |
| `taskData`       | `unknown`                    | Arbitrary data passed in options.                    |
| `resourceLimits` | `TaskResourceLimits \| null` | Resource limits configured for the task.             |
| `onerror`        | `((ev: any) => any) \| null` | Handler for error events.                            |
| `onmessage`      | `((ev: any) => any) \| null` | Handler for incoming messages.                       |
| `onmessageerror` | `((ev: any) => any) \| null` | Handler for message deserialization errors.          |

#### Methods

- `postMessage(message: unknown): void`: Calls the runner's registered `onMessage` handler. Messages sent before registration are not queued.
- `done(): Promise<void>`: Observes completion of synchronous entry evaluation. It does not await promises returned or scheduled by entry code. Resolution/evaluation failures emit `error` and exit code 1; the current implementation can still resolve `done()` after reporting them. Observe `error` and `exit` events for the execution outcome. Termination may skip evaluation.
- `terminate(): Promise<number>`: Marks the task terminated, attempts to cancel queued work, and skips evaluation if it has not begun. It cannot preempt JavaScript already executing on the event loop. Once complete, the implementation returns the recorded numeric exit code directly; awaiting it also works.
- `cancel(): boolean`: Issues `uv_cancel` on the underlying libuv work request before execution starts.
- `reference(): void`: Sets the reference flag to true.
- `unreference(): void`: Sets the reference flag to false. These methods currently update bookkeeping; they do not ref/unref a libuv handle.
- `addEventListener(type: string, listener: (ev: any) => any): void`
- `removeEventListener(type: string, listener: (ev: any) => any): void`
- `dispatchEvent(event: unknown): void`

#### Events and runner bindings

Task uses `addEventListener()` and `onmessage` / `onerror` properties, rather than a Node.js `EventEmitter.on()` API. Message and error events carry `{ type, data }`; exit events carry `{ type, code }`. The event registry also accepts a `messageerror` handler.

Inside an entry, `postMessage(value)` sends a message to the host listeners, `onMessage(callback)` registers the inbox callback, and `taskData` exposes the supplied data. This is an in-environment callback interface, not subprocess IPC.

### `TaskSignal`

Wakeup primitive wrapping libuv `uv_async_t` to signal the Node.js event loop from arbitrary background threads.

```ts
import { TaskSignal } from "repository";

const signal = new TaskSignal(() => {
  console.log("Woken up on the event loop");
});

signal.send();
signal.close();
```
