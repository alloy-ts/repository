# Task API

The `Task` API provides background execution primitives in Node.js backed by `libuv`'s thread pool and event-loop integration, mirroring Node's worker and task threading models.

## Overview

A `Task` object executes specified JavaScript or TypeScript code, file scripts, or Git repository object blobs asynchronously. It leverages libuv (`uv_queue_work` / `uv_async_t`) for thread pool processing and event loop messaging via `postMessage` and `onmessage`.

Submodules under `task`:

- **`thread`** ([thread.md](./thread.md)): Direct OS thread handles (`Thread`, `Builder`, `JoinHandle`) and thread-pool wrappers (`ThreadPool`).
- **`scheduler`** ([scheduler.md](./scheduler.md)): Task scheduling and dispatching (`Scheduler`, `TaskHandle`) managing worker execution pools.
- **`promise`** ([promise.md](./promise.md)): Native Promise bridging and deferred resolution (`asyncPlus100`, `processBuffer`, `processBufferOnPool`).
- **`process`** ([process.md](./process.md)): Async child process execution, stdio streaming, cancellation tokens, and timeouts.

## Usage

```ts
import { Task } from "repository";

const task = new Task({
  entries: "postMessage('hello from task');",
  name: "worker-task",
  taskData: { key: "value" },
});

console.log(`Task worker running with thread ID: ${task.threadId}`);

task.onmessage = (event) => {
  console.log("Message from task:", event.data);
};

// Send message to task
task.postMessage("ping");

// Await completion
await task.done();

// Or terminate early
await task.terminate();
```

## API Reference

### `Task` Class

#### Constructor

```ts
new Task(options: TaskOptions)
```

Creates a new `Task` instance that executes the configured entries.

#### Properties

| Property         | Type                         | Description                                          |
| ---------------- | ---------------------------- | ---------------------------------------------------- |
| `threadId`       | `number`                     | Unique integer thread/task identifier.               |
| `threadName`     | `string`                     | The task's assigned name.                            |
| `hasRef`         | `boolean`                    | Whether the task keeps the process event loop alive. |
| `terminated`     | `boolean`                    | Whether `terminate()` has been called.               |
| `taskData`       | `unknown`                    | Arbitrary data passed in options.                    |
| `resourceLimits` | `TaskResourceLimits \| null` | Resource limits configured for the task.             |
| `onerror`        | `((ev: any) => any) \| null` | Handler for error events.                            |
| `onmessage`      | `((ev: any) => any) \| null` | Handler for incoming messages.                       |
| `onmessageerror` | `((ev: any) => any) \| null` | Handler for message deserialization errors.          |

#### Methods

- `postMessage(message: unknown): void`: Sends a message to the task runner scope.
- `done(): Promise<void>`: Resolves when the task entries complete or rejects on failure.
- `terminate(): Promise<number>`: Requests termination and resolves with the final exit code.
- `cancel(): boolean`: Issues `uv_cancel` on the underlying libuv work request before execution starts.
- `reference(): void`: Keeps the event loop alive while running.
- `unreference(): void`: Allows event loop exit while running.
- `addEventListener(type: string, listener: (ev: any) => any): void`
- `removeEventListener(type: string, listener: (ev: any) => any): void`
- `dispatchEvent(event: unknown): void`

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
