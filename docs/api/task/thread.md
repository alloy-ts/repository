# Thread API

The `thread` module provides low-level OS threading primitives, thread-pool management, and thread configuration for CPU-bound concurrent operations.

## Overview

The thread module exposes thread handles, builder configurations, and threading utilities to spawn and monitor background threads, manage concurrency limits, and query hardware parallelism.

## API Reference

### Functions

- `availableParallelism(): number`: Returns the number of logical cores available to the current process.
- `currentThreadId(): bigint`: Returns the numeric identifier of the calling thread.
- `isMainThread(): boolean`: Returns `true` if executed on the main event-loop thread.

### Classes

#### `ThreadPool`
Manages a fixed or dynamic pool of worker threads for background execution.

```ts
import { ThreadPool } from "repository";

const pool = new ThreadPool({ size: 4 });
```

#### `ThreadBuilder`
Configures stack size, thread name, and execution environment before spawning a thread.
