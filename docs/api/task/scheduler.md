# Scheduler API

The `Scheduler` class provides scheduling, execution queue management, and task concurrency control across worker threads.

## Overview

`Scheduler` manages task queues, priority scheduling, and background thread dispatching with event-loop wakeups.

## Usage

```ts
import { Scheduler, Task } from "repository";

const scheduler = new Scheduler();
scheduler.submit(new Task("worker.js"));
```

## API Reference

### `Scheduler` Class

- `constructor(options?: SchedulerOptions)`: Initializes a task scheduler.
- `submit(task: Task)`: Submits a task for scheduled execution.
- `cancel(taskId: string)`: Cancels a pending scheduled task.
