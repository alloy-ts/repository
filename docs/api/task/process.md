# Process API

Async process spawning, child management, stdio piping, and cooperative cancellation built on Tokio.

## Overview

The `process` module provides utilities for executing subprocesses asynchronously, capturing outputs, streaming stdin/stdout/stderr, and handling cancellation tokens and execution timeouts.

## Classes and Interfaces

### `Command`

Builder for executing child processes asynchronously.

#### Methods

- `constructor(program: string)`: Creates a new command invocation.
- `arg(arg: string): Command`: Adds an argument.
- `args(args: string[]): Command`: Adds multiple arguments.
- `cwd(dir: string): Command`: Sets working directory.
- `env(key: string, val: string): Command`: Sets environment variable.
- `run(): Promise<CommandResult>`: Executes the command and awaits completion.

### `CancellationToken`

Allows cooperative cancellation across asynchronous tasks and operations.

#### Methods

- `constructor()`: Creates a new cancellation token.
- `cancel(): void`: Triggers cancellation.
- `isCancelled(): boolean`: Checks if cancellation has been requested.
