# ThreadPool API

The `ThreadPool` class manages a pool of worker threads capable of executing tasks asynchronously in background threads.

## Overview

`ThreadPool` provides low-level control over work requests and threads via libuv threadpool integration.

## Usage

```ts
import { ThreadPool } from "repository";

const pool = new ThreadPool();
pool.exec(() => {
  // Executed on worker thread
});
```

## API Reference

### `ThreadPool` Class

- `constructor(numThreads?: number)`: Initializes a thread pool.
- `exec(task: Function, callback?: Function)`: Queues a task on the thread pool.
- `run(task: Function, after: Function)`: Runs work on worker threads with an after-work completion callback on the main loop.
