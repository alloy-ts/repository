# Promise Utilities API

The promise module exposes NAPI-RS bridge utilities for bridging JavaScript Promises with Rust Futures and offloading buffer operations across thread pools.

## Overview

Key functionality:
- Awaiting JavaScript promises asynchronously inside Rust execution contexts.
- Settling JavaScript promises via deferred handles from background threads.
- Offloading heavy buffer mutations to the libuv thread pool.

## Usage

```ts
import {
  asyncPlus100,
  processBuffer,
  processBufferOnPool,
  promiseCallback,
} from "repository";

const result = await asyncPlus100(Promise.resolve(42));
console.log(result); // 142

const buf = Buffer.from("hello world");
const processed = await processBufferOnPool(buf);
```

## API Reference

### Functions

##### `asyncPlus100(p: Promise<number>): Promise<number>`
Demonstrates awaiting a JavaScript promise inside Rust.

##### `promiseCallback(promise: Promise<number>, cb: (val: number) => void): void`
Schedules a callback to be invoked when the given promise resolves.

##### `processBuffer(buffer: Buffer): Promise<Buffer>`
Processes buffer bytes asynchronously on the event loop.

##### `processBufferOnPool(buffer: Buffer): Promise<Buffer>`
Dispatches buffer processing to the worker thread pool and settles the Promise on the main thread upon completion.
