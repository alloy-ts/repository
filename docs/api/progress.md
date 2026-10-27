# Progress API

Progress reporting for long-running network operations — `fetch`, `clone`, and
pack transfer. This library is **event-based**: a JS callback receives a stream
of `ProgressEvent`s as work proceeds, rather than git2's single flat `Progress`
snapshot.

Two surfaces are exposed:

- `ProgressEvent` — the structured payload delivered to the `progress` callback
  of `Remote.fetch`, `Repository.clone`, and `Pack.toBuffer` /
  `writeToDirectory`.
- `RemoteProgress` — a parser for raw server sideband lines, for callers who
  read the protocol directly.

## `ProgressEvent`

Passed to the `progress` callback of fetch/clone/pack operations.

```ts
type ProgressCallback = (event: ProgressEvent) => void;
```

```ts
interface ProgressEvent {
  name?: string; // current phase, e.g. "Receiving objects", "Resolving deltas"
  step?: number; // items processed so far
  max?: number; // expected total items, if known
  percent?: number; // 0–100, derived from step/max when both are present
  message?: string; // free-form log line for info/success/failure events
  level?: MessageLevel; // severity of `message`
  unit?: string; // unit of step/max, e.g. "objects"; only on a phase's open event
}
```

```ts
const enum MessageLevel {
  Info, // informational progress
  Success, // step completed
  Failure, // step failed
}
```

Fields are **optional** and appear as a phase evolves: a phase opens with a
`name` + `max` + `unit` (`init`), then streams `step`/`percent` updates, and may
emit `message`/`level` log lines. `unit` is present only on the opening `init`
event; later updates omit it.

## `RemoteProgress`

Parse a raw progress `line` as sent by the remote (e.g. from a custom transport)
into structured form.

```ts
RemoteProgress.fromBytes(line: string): RemoteProgress | null;
```

Returns `null` when the line carries no recognizable progress (keep-alive
packets, plain sideband text).

```ts
interface RemoteProgress {
  action: string; // the action, e.g. "Receiving objects"
  percent?: number; // 0–100, if the server reported one
  step?: number; // items processed so far
  max?: number; // expected total items
  receivedBytes?: number; // total bytes transferred (git2 `Progress.received_bytes`)
  throughput?: number; // bytes per second, from the `| x MiB/s` tail
}
```

`receivedBytes` / `throughput` are parsed from the trailing throughput portion
of the line (e.g. `, 1.23 MiB | 4.56 MiB/s`) using git's 1024-base units
(`B`/`KiB`/`MiB`/`GiB`/`TiB`/`PiB`/`EiB`). They are `null` when the server
omitted throughput — the common case for phase lines without a rate.

## git2 `Progress` → `ProgressEvent`

git2 delivers a flat
`Progress { total_objects, indexed_objects,
received_objects, local_objects, total_deltas, indexed_deltas, received_bytes }`
snapshot per update. Ours is phase-keyed: read the same counters off the event
whose `name` matches the phase:

| git2 `Progress` field | `ProgressEvent` (filter by `name`)                  |
| --------------------- | --------------------------------------------------- |
| `total_objects`       | `max` when `name === "Receiving objects"`           |
| `received_objects`    | `step` when `name === "Receiving objects"`          |
| `indexed_objects`     | `step` when `name === "Indexing objects"`           |
| `local_objects`       | `step` when `name === "linking local objects"`      |
| `total_deltas`        | `max` when `name === "Resolving deltas"`            |
| `indexed_deltas`      | `step` when `name === "Resolving deltas"`           |
| `received_bytes`      | `RemoteProgress.receivedBytes` (parse the raw line) |

The phase-keyed shape is more JS-friendly than git2's flat struct because it
does not conflate phases; reconstruct a flat snapshot in JS if a single object
is needed.

## Gaps vs git2

- **Cancellation.** git2's progress callback returns `bool` to abort the
  transfer. gix's `Progress` trait has no such hook; cancellation here flows
  through a separate interrupt/signal handle, not the callback. The `progress`
  callback returns `void`.
- **Raw byte counts in `ProgressEvent`.** gix's progress tree only ever counts
  objects (`unit` is effectively always `"objects"`), and the transport byte
  rate is only recoverable from the raw sideband text via `RemoteProgress`, not
  from the `ProgressEvent` stream.
- **`Indexer` / `OdbPackwriter`.** git2's pack _writer/indexer_ (and its
  `IndexerProgress`) have no equivalent in this library — that is a pack-writing
  feature, not progress reporting.

## Working example

```ts
import { RemoteProgress } from "@alloy-ts/repository";

// Parse a server progress line, including throughput
RemoteProgress.fromBytes("Receiving objects:  50% (100/200), 1.23 MiB | 4.56 MiB/s");
// {
//   action: "Receiving objects", percent: 50, step: 100, max: 200,
//   receivedBytes: 1289748.48, throughput: 4781506.56,
// }

// A phase line without throughput
RemoteProgress.fromBytes("Resolving deltas: 30% (30/100)");
// { action: "Resolving deltas", percent: 30, step: 30, max: 100,
//   receivedBytes: undefined, throughput: undefined }

// Drive a fetch with a progress callback
await remote.fetch({
  progress: (e) => {
    if (e.name === "Receiving objects") {
      console.log(`${e.percent ?? 0}% (${e.step}/${e.max}) — unit: ${e.unit}`);
    }
    if (e.level === MessageLevel.Failure) console.error(e.message);
  },
});
```
