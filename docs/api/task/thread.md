# Thread API

The `Thread` class provides access to the underlying OS and runtime thread state, wrapping Rust's `std::thread::Thread` and libuv execution handles.

## Overview

The `Thread` module provides:
- Access to the currently executing thread's identifier and metadata.
- Thread naming and introspection.
- Park and unpark thread synchronization mechanisms.

## Usage

```ts
import { Thread } from "repository";

const current = Thread.current();
console.log("Current thread name:", current.name);
```

## API Reference

### `Thread` Class

#### Static Methods

##### `Thread.current(): Thread`
Returns a handle to the currently running thread.

#### Properties

| Property | Type | Description |
| --- | --- | --- |
| `name` | `string \| null` | The name of the thread, if configured. |

#### Methods

##### `unpark(): void`
Atomically makes the token available if it wasn't already.

##### `id(): bigint`
Unique numeric thread identifier.
