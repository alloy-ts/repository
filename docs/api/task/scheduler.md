# Scheduler API

The `Scheduler` class coordinates tasks across thread workers and asynchronous execution queues.

## Overview

A `Scheduler` manages concurrent task units, scheduling them across worker threads and providing progress tracking via `TaskHandle`.

## Usage

```ts
import { Scheduler } from "repository";

const scheduler = new Scheduler();

// Submit computation to scheduler
const req = scheduler.submit(10);

// Await completion via Promise
const result = await req.promise();
```

## Classes

### `Scheduler`

#### Constructor

```ts
new Scheduler(kind?: SchedulerKind)
```

Constructs a new task scheduler. `kind` selects the underlying dispatch backend (`LibuvWork` or `DirectThread`).

#### Methods

- `submit(value: number): TaskHandle`: Submits an integer task and returns its progress handle.
- `queueTask(options: TaskOptions): Task`: Enqueues an execution task.
- `submitTask(code: string): Promise<any>`: Runs arbitrary code asynchronously on the scheduler.

### `TaskHandle`

Represents an in-flight computation scheduled on a `Scheduler`.

#### Methods

- `promise(): Promise<number>`: Resolves when execution completes.
- `cancel(): boolean`: Attempts to cancel the task before execution starts.
