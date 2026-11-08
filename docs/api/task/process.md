# Process API

Async process spawning, child management, stdio piping, and cooperative cancellation built on Tokio.

## Overview

The `process` module provides utilities for executing subprocesses asynchronously, capturing outputs, streaming stdin/stdout/stderr, and handling cancellation tokens and execution timeouts.

## Classes and Interfaces

### `Process`

A spawnable, cancellable, byte-bounded subprocess with IPC support.

#### Constructor

```ts
new Process(target?: string | URL | { href: string } | TaskOptions | CommandOptions, cancellation?: CancellationToken)
```

Constructs a new `Process`. When instantiated with an executable path, URL, URL object, or Task object, it automatically starts a subprocess.

#### Properties

- `pid: number | null`: Process identifier of the child process.
- `connected: boolean`: Whether the process is connected to its IPC channel.
- `killed: boolean`: Whether the process has exited or been signaled.
- `exitedAfterDisconnect: boolean`: Whether the process exited after a manual `disconnect()`.

#### Methods

- `isDead(): boolean`: Returns whether the process has terminated.
- `isConnected(): boolean`: Returns whether the IPC channel is open.
- `send(message: unknown): boolean`: Sends an IPC message to the child process.
- `disconnect(): void`: Closes the IPC channel.
- `kill(signal?: string): boolean`: Terminates the child process (default `'SIGTERM'`).
- `destroy(signal?: string): boolean`: Alias for `kill`.
- `on(event: string, listener: (...args: any[]) => void): void`: Subscribes to child events (`exit`, `message`, `error`, `disconnect`).
- `addEventListener(event: string, listener: (...args: any[]) => void): void`: Alias for `on`.
- `run(): Promise<CommandResult>`: Executes the command and resolves with captured output or execution failure.

### `CancellationToken`

Allows cooperative cancellation across asynchronous tasks and operations.

#### Methods

- `constructor()`: Creates a new cancellation token.
- `cancel(): void`: Triggers cancellation.
- `isCancelled(): boolean`: Checks if cancellation has been requested.
