# Threaded Task API

The `Task` class provides threaded background execution in Node.js via worker threads (`node:worker_threads`), enabling laborious script processing to run on a separate thread without blocking the main event loop.

## Overview

A `Task` object runs a named JavaScript or TypeScript file in a dedicated background worker thread, mirroring the Web Worker / `Worker` model. Communication between the main thread and the task thread uses message passing via structured cloning (`postMessage` / `onmessage`).

## Usage

```ts
import { Task } from "repository";

const myTask = new Task("worker.js", { name: "background-worker" });

console.log(`Task worker running on thread ID: ${myTask.threadId}`);

// Listen for messages from the task thread
myTask.onmessage = (e) => {
  console.log("Message received from task:", e.data);
};

// Send messages to the task thread
myTask.postMessage(["input1", "input2"]);

// Alternatively, use EventTarget-style listeners
myTask.addEventListener("message", (e) => {
  console.log("EventListener message:", e.data);
});

// Terminate the task when work is finished
myTask.terminate();
```

## API Reference

### `Task` Class

#### Constructor

```ts
new Task(script: string, options?: TaskOptions)
```

Creates a new `Task` instance that executes the specified JavaScript/TypeScript file in a background worker thread.

- `script`: Path to the worker script file.
- `options`: Optional configuration object.

#### Properties

| Property | Type | Description |
| --- | --- | --- |
| `threadId` | `number` | Unique integer thread identifier assigned to the worker thread. |
| `onerror` | `((ev: ErrorEvent) => any) \| null` | Event handler called when an error occurs in the worker thread. |
| `onmessage` | `((ev: MessageEvent) => any) \| null` | Event handler called when a message is received from the worker thread. |
| `onmessageerror` | `((ev: MessageEvent) => any) \| null` | Event handler called when message deserialization fails. |

#### Methods

##### `postMessage(message: any, transfer?: any[]): void`
Clones `message` and transmits it to the task worker's global scope.

##### `addEventListener(type: string, listener: (ev: any) => any, options?: any): void`
Appends an event listener for events of the specified `type` (`"message"`, `"error"`, or `"messageerror"`).

##### `removeEventListener(type: string, listener: (ev: any) => any, options?: any): void`
Removes a previously registered event listener.

##### `dispatchEvent(event: Event \| { type: string; [key: string]: any }): boolean`
Dispatches a synthetic event to the registered handlers and listeners.

##### `ref(): void`
Opposite of `unref()`. Prevents the process from exiting while the worker thread is active.

##### `unref(): void`
Allows the host process to exit if this worker thread is the only active handle remaining in the event loop.

##### `terminate(): void`
Terminates the worker thread immediately and frees associated resources.

### `TaskOptions` Interface

```ts
interface TaskOptions {
  name?: string; // Optional thread name for debugging / profiling
}
```
