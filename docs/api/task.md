# Threaded Tasks API

The **Threaded Task API** makes it possible to run script operations in background worker threads separate from the main execution thread of a Node.js application. Laborious processing can be performed in a separate thread without blocking or slowing down the main thread.

A task is an object constructed using `new Task(script, options?)` (also exported as `Worker`) that runs a named JavaScript/TypeScript file in a separate thread backed by Node.js worker threads (`node:worker_threads`).

## `TaskOptions`

Optional settings passed to the `Task` constructor.

```ts
interface TaskOptions {
  /**
   * Optional name for the worker thread (forwarded to `Worker`'s `name` option).
   * Useful for debugging and profiling.
   */
  name?: string;
}
```

## `Task` Class (`Worker` Alias)

Represents a threaded background task instance. `Worker` is exported as a synonym for `Task` for users familiar with the Web Worker API.

### Constructor

```ts
const myTask = new Task(script: string, options?: TaskOptions);
```

- `script`: The script file path executed by the worker thread.
- `options`: Optional configuration settings (`TaskOptions`).

### Properties

| Property | Type | Description |
| --- | --- | --- |
| `script` | `string` | The script file path assigned to the task. |
| `threadId` | `number` | The process-unique `threadId` assigned to the worker thread. |
| `terminated` | `boolean` | `true` if `terminate()` has been called on this task instance. |

### Event Handlers

| Handler | Type | Description |
| --- | --- | --- |
| `onmessage` | `((ev: any) => void) \| null` | Handler invoked when a message is received from the worker thread. |
| `onerror` | `((ev: any) => void) \| null` | Handler invoked when an unhandled error occurs in the worker thread. |
| `onmessageerror` | `((ev: any) => void) \| null` | Handler invoked when a message deserialization error occurs. |

### Methods

#### `postMessage`
```ts
myTask.postMessage(message: any, transfer?: Transferable[]): void;
```
Clones `message` via structured clone and transmits it to the task's worker thread. `transfer` can optionally contain transferable objects (e.g. `ArrayBuffer`, `MessagePort`).

#### `terminate`
```ts
myTask.terminate(): Promise<number>;
```
Stops execution in the worker thread as soon as possible and returns a promise that resolves with the exit code.

#### `ref` / `unref`
```ts
myTask.ref(): void;
myTask.unref(): void;
```
- `ref()`: Keeps the Node.js event loop active until the task worker thread exits.
- `unref()`: Allows the Node.js process to exit if this task is the only active handle remaining.

#### Event Listener Methods
```ts
myTask.addEventListener(type: string, listener: (ev: any) => void): void;
myTask.removeEventListener(type: string, listener: (ev: any) => void): void;
myTask.dispatchEvent(event: { type: string; [key: string]: any }): boolean;
```
Registers, unregisters, or dispatches synthetic events on the task instance.

---

## Working Example

```ts
import { Task } from "@alloy-ts/repository";

// Construct a new task running a worker script in a background thread
const myTask = new Task("worker.js", { name: "background-calculator" });

console.log(`Task created on threadId: ${myTask.threadId}`);

// Handle incoming messages from the worker task
myTask.onmessage = (e) => {
  console.log("Message received from task:", e.data);
};

// Listen for errors
myTask.onerror = (e) => {
  console.error("Task error:", e.message || e.error);
};

// Post data to the worker thread
myTask.postMessage([10, 20]);

// Clean up when complete
await myTask.terminate();
```
