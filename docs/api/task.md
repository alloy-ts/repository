# Tasks API

The (Threaded) Tasks API makes it possible to run script operations in background worker threads separate from the main execution thread of an application. Laborious processing can be performed in a separate thread, keeping the main (UI/server) event loop unblocked.

A task is created using the `Task` constructor (`new Task("task.js")`) and runs a named JavaScript or TypeScript file inside a worker thread (`node:worker_threads`).

## `Task` Class

### Constructor

`new Task(script: string, options?: TaskOptions)`

Constructs a new threaded task executing `script`.

```ts
interface TaskOptions {
  name?: string; // Optional thread name for debugging / profiling
}
```

### Properties

| Property         | Type                                   | Description                                             |
| ---------------- | -------------------------------------- | ------------------------------------------------------- |
| `threadId`       | `number` (readonly)                    | Monotonic integer identifier for the referenced thread. |
| `threadName`     | `string \| undefined` (readonly)       | Optional thread name.                                   |
| `script`         | `string` (readonly)                    | Script path executed by this task.                      |
| `terminated`     | `boolean` (readonly)                   | Whether `terminate()` has been called.                  |
| `onmessage`      | `((ev: MessageEvent) => void) \| null` | Handler for `message` events received from the task.    |
| `onerror`        | `((ev: ErrorEvent) => void) \| null`   | Handler for uncaught `error` events in the task.        |
| `onmessageerror` | `((ev: MessageEvent) => void) \| null` | Handler for `messageerror` events.                      |

### Methods

| Method                | Signature                                                   | Description                                                                   |
| --------------------- | ----------------------------------------------------------- | ----------------------------------------------------------------------------- |
| `postMessage`         | `(message: any, transfer?: any[]) => void`                  | Posts a message to the task thread (cloned or transferred).                   |
| `terminate`           | `() => void`                                                | Terminates the worker thread immediately.                                     |
| `ref`                 | `() => void`                                                | Keeps the event loop active for this task thread.                             |
| `unref`               | `() => void`                                                | Allows the process event loop to exit if this task is the only active handle. |
| `addEventListener`    | `(type: string, listener: Function, options?: any) => void` | Registers an event listener on the task.                                      |
| `removeEventListener` | `(type: string, listener: Function, options?: any) => void` | Removes an event listener from the task.                                      |
| `dispatchEvent`       | `(event: any) => boolean`                                   | Dispatches a synthetic event to registered listeners.                         |

## Global Helper Functions

| Function                | Signature                                                  | Description                                                                  |
| ----------------------- | ---------------------------------------------------------- | ---------------------------------------------------------------------------- |
| `isMainThread`          | `() => boolean`                                            | Returns `true` if executed on the main thread, `false` inside a task thread. |
| `getThreadId`           | `() => number`                                             | Returns the thread ID for the current thread context.                        |
| `setEnvironmentData`    | `(key: any, value: any) => void`                           | Sets environment data shared across task threads in the process.             |
| `getEnvironmentData`    | `(key: any) => any`                                        | Retrieves environment data shared across task threads.                       |
| `createMainThreadPort`  | `(threadId: number) => MessagePort`                        | Creates a `MessagePort` connecting a target thread to the main thread.       |
| `destroyMainThreadPort` | `(threadId: number) => void`                               | Closes and cleans up a main thread `MessagePort`.                            |
| `postMessageToThread`   | `(threadId: number, value: any, transfer?: any[]) => void` | Posts a message directly to a target thread's main port.                     |

## Task Contexts & Enums

- `TaskType`: String-valued enum representing task types (`Dedicated`, `Shared`, `Service`).
- `DedicatedTaskGlobalScope`: Context class for dedicated worker tasks.
- `SharedTaskGlobalScope`: Context class for shared worker tasks.
- `ServiceTaskGlobalScope`: Context class for service worker tasks.

## Example Usage

```ts
import { Task } from "@alloy-ts/repository";

const myTask = new Task("worker.cjs", { name: "data-processor" });

// Listen for messages from the task
myTask.onmessage = (e) => {
  console.log("Message received from task:", e.data);
};

// Listen for uncaught worker errors
myTask.onerror = (e) => {
  console.error("Worker error:", e.message);
};

// Send data to the task
myTask.postMessage({ action: "compute", data: [10, 20, 30] });

// Clean up when done
myTask.terminate();
```
