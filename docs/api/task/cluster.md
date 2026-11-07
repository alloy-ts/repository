# Cluster API

Process cluster management, task forking, load distribution, and IPC communication.

## Overview

The `cluster` module allows easy creation of child processes that can distribute workloads among tasks.

## Classes and Interfaces

### `Cluster`

Cluster manager providing cluster status, settings, worker lifecycle management, and event handling.

#### Properties and Getters

- `isPrimary` / `isMaster`: `boolean` - True if current process is primary (no `NODE_UNIQUE_ID`).
- `isWorker` / `isTask`: `boolean` - True if current process is a worker.
- `schedulingPolicy`: `number` - Current scheduling policy (`SCHED_RR = 2`, `SCHED_NONE = 1`).
- `settings`: `ClusterSettings` - Configuration settings for forked processes.
- `workers` / `tasks`: `Record<string, Worker>` - Map of active workers keyed by worker ID.
- `worker` / `task`: `Worker | null` - Reference to current worker if running inside a worker process.

#### Methods

- `fork(env?: Record<string, string>): Worker`: Spawns a new worker process.
- `setupPrimary(settings?: ClusterSettings): void`: Updates cluster settings.
- `setupMaster(settings?: ClusterSettings): void`: Deprecated alias for `setupPrimary`.
- `disconnect(callback?: () => void): void`: Disconnects all workers.
- `on(event: string, listener: (...args: any[]) => void)`: Listens for cluster events.
- `removeListener(event: string, listener: (...args: any[]) => void)`: Removes listener.
- `emit(event: string, ...args: any[]): boolean`: Emits an event.

### `Worker`

Represents an individual worker / task process.

#### Properties and Getters

- `id`: `number` - Unique numeric identifier.
- `process`: `ChildProcess` - ChildProcess representation with `pid`, `connected`, `killed`.
- `exitedAfterDisconnect`: `boolean | undefined` - True if exited voluntarily after disconnect.

#### Methods

- `send(message: unknown): boolean`: Sends message over IPC channel.
- `disconnect(): void`: Gracefully disconnects the worker.
- `kill(signal?: string): void`: Terminates worker process.
- `destroy(signal?: string): void`: Alias for `kill()`.
- `isConnected(): boolean`: Returns whether IPC is connected.
- `isDead(): boolean`: Returns whether process has terminated.
- `on(event: string, listener: (...args: any[]) => void)`: Listens for worker events.
- `emit(event: string, ...args: any[])`: Emits a worker event.
