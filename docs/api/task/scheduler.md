# Scheduler API

The `scheduler` module coordinates task execution, priorities, deadlines, and task-graph workflows across threads.

## Overview

Task scheduling allows scheduling work items with varying priority levels, cancellations, timeouts, and dependency graphs.

## API Reference

### Classes

#### `Scheduler`
Core scheduling engine that queues and dispatches tasks based on priority, resource limits, and dependencies.

- `schedule(task: Task, options?: ScheduleOptions): Promise<void>`
- `cancel(taskId: string | number): boolean`
- `drain(): Promise<void>`

### Interfaces

#### `ScheduleOptions`
- `priority?: number`: Priority level for execution ordering.
- `timeoutMs?: number`: Maximum time allowed before cancellation.
- `deadline?: Date`: Absolute deadline for task completion.
