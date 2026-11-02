# Scheduler API

The `Scheduler` class coordinates concurrent work execution across thread pools and local event loop execution queues.

## Overview

The scheduler unifies execution backends:
- `Libuv`: Node.js's default libuv thread pool (`uv_queue_work`).
- `ThreadPool`: Multi-threaded worker pool powered by the `futures` crate.
- `Local`: Dedicated single-threaded driver queue that accepts non-`Send` tasks.

## Usage

```ts
import { Scheduler, SchedulerKind } from "repository";

const scheduler = new Scheduler(SchedulerKind.Libuv);

const handle = scheduler.submit(
  (progress) => {
    // Background work
  },
  () => {
    // Completion callback
  }
);
```

## API Reference

### `SchedulerKind` Enum

| Variant | Value | Description |
| --- | --- | --- |
| `Libuv` | `"libuv"` | Standard libuv thread pool. |
| `ThreadPool` | `"threadPool"` | Futures M:N multi-threaded executor. |
| `Local` | `"local"` | Dedicated single-threaded driver. |

### `Scheduler` Class

#### Constructor

```ts
new Scheduler(kind?: SchedulerKind, options?: SchedulerOptions)
```

#### Methods

##### `submit(work: (progress: (pct: number) => void) => void, after: () => void): TaskHandle`
Submits work to the scheduler with optional progress notifications back to the event loop.

##### `submitWork(work: () => any): Promise<any>`
Submits an async computation returning a Promise.
