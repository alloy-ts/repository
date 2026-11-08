# Cluster API (draft)

Source: [src/task/cluster.rs](../../../sources/task/src/cluster.rs).

`Cluster` groups independent child executions and keeps cancellation registrations indexed by a stable task ID. Each child is a [Process](./process.md), with its own address space. This draft does not provide Node cluster IPC, shared server ports, handle passing, scheduling policies, lifecycle events, or graceful disconnect.

## Usage

```ts
import { Cluster } from "repository";

const cluster = new Cluster();
const child = cluster.fork({
  program: process.execPath,
  args: ["-e", "console.log(process.env.ALLOY_CLUSTER_TASK_ID)"],
  timeoutMs: 5000,
});
const id = child.id!;
try {
  const result = await child.run();
  console.log(id, result.output?.stdout.toString("utf8"));
} finally {
  cluster.remove(id);
}
```

## API

| Member | Return | Behavior |
| --- | --- | --- |
| `new Cluster()` | `Cluster` | Creates an empty, instance-local registry. |
| `fork(options: CommandOptions)` | `Process` | Schedules one child execution. Its read-only `id` identifies this registration. |
| `taskIds` | `number[]` | A sorted snapshot of registered task IDs. |
| `cancel(id: number)` | `boolean` | Requests cancellation; returns false for an unknown ID. Keeps the registration. |
| `remove(id: number)` | `boolean` | Removes the registration and requests cancellation if the child is still running. Returns false for an unknown ID. |
| `cancelAll()` | `void` | Requests cancellation of every registered child. Keeps registrations. |

`fork()` requires an explicit command descriptor; it does not recursively fork the current Node application. The `program` can be an executable path/name or local file URL string. Other options, output caps, timeout errors, and result collection follow Process.

IDs increase within each Cluster instance and are not reused after removal. They are not OS PIDs and may overlap between separate Cluster instances. The child receives its ID as `ALLOY_CLUSTER_TASK_ID`, overriding a descriptor-supplied entry with that name. Other environment variables are inherited or overridden through `env`.

A command may fail to spawn after its ID is assigned: await `child.run()` to observe the error. No placeholder PID or successful lifecycle event is synthesized.

## Lifetime and cleanup

Completion does not automatically remove a registration. Call `remove(child.id!)` after collecting the result. Cancellation and removal do not await cleanup; await the child's original `run()` promise for the observed outcome.

Dropping the native Cluster requests cancellation of all registered children. Dropping a Process also requests its cancellation. Garbage collection timing is nondeterministic, so use explicit cancellation/removal when cleanup timing matters. Internal cancellation registrations remain alive until the execution, cleanup, and every Process/Cluster owner have finished; the final owner releases the registry entry.

Keep the returned Process objects reachable while collecting results. `run()` collects each execution only once. Sending messages between children, forwarding network connections, and restarting failed children require future implementation or application-level coordination.
