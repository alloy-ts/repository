# Cluster API

The cluster module enables creation of child worker processes that share server ports, distributing workloads across CPU cores.

## Overview

The `cluster` module provides an interface mirroring Node's `node:cluster` API:

- Worker processes are spawned with `cluster.fork([env])`.
- Workers can communicate with the primary process via IPC messages.
- TCP/HTTP server connections can be shared or distributed across workers.

## Usage

```ts
import cluster from "@alloy-ts/task/cluster";
import http from "node:http";
import { availableParallelism } from "node:os";
import process from "node:process";

const numCPUs = availableParallelism();

if (cluster.isPrimary) {
  console.log(`Primary ${process.pid} is running`);

  // Fork workers.
  for (let i = 0; i < numCPUs; i++) {
    cluster.fork();
  }

  cluster.on("exit", (task, code, signal) => {
    console.log(`task ${task.id} died (${signal || code})`);
  });
} else {
  // Workers share port 8000
  http
    .createServer((req, res) => {
      res.writeHead(200);
      res.end("hello world\n");
    })
    .listen(8000);

  console.log(`task ${process.pid} started`);
}
```

## Classes and Interfaces

### `Cluster`

#### Properties

| Property           | Type                     | Description                                           |
| ------------------ | ------------------------ | ----------------------------------------------------- |
| `isPrimary`        | `boolean`                | `true` if current process is the primary coordinator. |
| `isMaster`         | `boolean`                | Deprecated alias for `isPrimary`.                     |
| `isWorker`         | `boolean`                | `true` if current process is a cluster worker.        |
| `istask`           | `boolean`                | Alias for `isWorker`.                                 |
| `schedulingPolicy` | `number`                 | Scheduling policy (`SCHED_RR` or `SCHED_NONE`).       |
| `schedNone`        | `number`                 | `cluster.SCHED_NONE` (1).                             |
| `schedRr`          | `number`                 | `cluster.SCHED_RR` (2).                               |
| `settings`         | `ClusterSettings`        | Advisory execution settings for forked processes.     |
| `tasks`            | `Record<string, number>` | Hash of active tasks keyed by ID.                     |
| `workers`          | `Record<string, number>` | Alias for `tasks`.                                    |
| `task`             | `ClusterTask \| null`    | Reference to current task if in worker process.       |
| `worker`           | `ClusterTask \| null`    | Alias for `task`.                                     |

#### Methods

- `fork(customEnv?: Record<string, string>): ClusterTask`: Spawns a new worker task.
- `setupPrimary(settings?: ClusterSettings): void`: Updates configuration defaults for future `fork()` calls.
- `setupMaster(settings?: ClusterSettings): void`: Deprecated alias for `setupPrimary`.
- `disconnect(callback?: () => void): void`: Gracefully disconnects all tasks in the cluster.
- `on(event: string, listener: (...args: any[]) => void): void`: Subscribes to cluster events (`fork`, `online`, `listening`, `message`, `disconnect`, `exit`, `setup`).
- `addEventListener(event: string, listener: (...args: any[]) => void): void`: Alias for `on`.

### `ClusterTask`

Represents an individual worker task.

#### Properties

| Property                | Type              | Description                                      |
| ----------------------- | ----------------- | ------------------------------------------------ |
| `id`                    | `number`          | Unique numerical identifier for this worker.     |
| `process`               | `Process \| null` | Backing subprocess instance.                     |
| `exitedAfterDisconnect` | `boolean`         | `true` if the task exited due to `disconnect()`. |

#### Methods

- `send(message: unknown): boolean`: Sends an IPC message to the worker.
- `kill(signal?: string): boolean`: Terminates the worker with the specified signal (`SIGTERM` by default).
- `destroy(signal?: string): boolean`: Alias for `kill`.
- `disconnect(): void`: Closes the IPC channel.
- `isConnected(): boolean`: Returns whether IPC channel is open.
- `isDead(): boolean`: Returns whether the worker process has terminated.
- `on(event: string, listener: (arg: any) => void): void`: Subscribes to task events (`message`, `disconnect`, `error`, `exit`, `online`, `listening`).
- `addEventListener(event: string, listener: (arg: any) => void): void`: Alias for `on`.
