# Threaded Tasks API

The **Threaded Task API** makes it possible to run script operations in background worker threads separate from the main execution thread of a Node.js application. Laborious processing can be performed in a separate thread without blocking or slowing down the main thread.

A task is an object constructed using `new Task(script, options?)` (also exported as `Worker`) that runs a named JavaScript/TypeScript file in a separate thread backed by Node.js worker threads (`node:worker_threads`).

## Constants

| Constant | Value | Description |
| --- | --- | --- |
| `K_MAX_YOUNG_GENERATION_SIZE_MB` | `0` | Resource limit index for max young generation size. |
| `K_MAX_OLD_GENERATION_SIZE_MB` | `1` | Resource limit index for max old generation size. |
| `K_CODE_RANGE_SIZE_MB` | `2` | Resource limit index for code range size. |
| `K_STACK_SIZE_MB` | `3` | Resource limit index for stack size. |
| `K_TOTAL_RESOURCE_LIMIT_COUNT` | `4` | Total resource limit count. |

## Environment Helper Functions

- `isMainThread(): boolean`: Returns `true` if execution is on the main application thread.
- `getEnvMessagePort(): MessagePort | null`: Returns the parent message port for the worker environment, or `null`.
- `isInternalThread(): boolean`: Returns `true` if running on an internal thread.
- `ownsProcessState(): boolean`: Returns `true` if the current thread owns process state.

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
| `threadName` | `string \| undefined` | Optional worker thread name. |
| `terminated` | `boolean` | `true` if `terminate()` has been called on this task instance. |
| `resourceLimits` | `TaskResourceLimits` | Configured resource limit settings. |

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

#### `terminate` / `stopThread`
```ts
myTask.terminate(): Promise<number>;
myTask.stopThread(): Promise<number>;
```
Stops execution in the worker thread as soon as possible and returns a promise that resolves with the exit code.

#### `ref` / `unref` / `hasRef`
```ts
myTask.ref(): void;
myTask.unref(): void;
myTask.hasRef(): boolean;
```
- `ref()`: Keeps the Node.js event loop active until the task worker thread exits.
- `unref()`: Allows the Node.js process to exit if this task is the only active handle remaining.
- `hasRef()`: Returns whether the task currently retains the event loop.

#### Profiling & Diagnostics

- `myTask.getHeapStatistics(): Promise<any>`: Fetches V8 heap statistics for the worker thread.
- `myTask.cpuUsage(prev?: any): Promise<{ user: number; system: number }>`: Gets CPU usage statistics.
- `myTask.takeHeapSnapshot(options?: any): Promise<any>`: Generates a V8 heap snapshot stream.
- `myTask.startCpuProfile(options?: any)` / `stopCpuProfile(profileId?: number)`: Controls CPU profiling.
- `myTask.startHeapProfile(options?: any)` / `stopHeapProfile()`: Controls heap profiling.

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
import { Task, isMainThread } from "@alloy-ts/repository";

console.log(`Is main thread: ${isMainThread()}`);

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
