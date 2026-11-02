# Promise & Task Async Bridging API

Provides asynchronous Promise settlement and bridging helpers connecting native threadpool workers to JavaScript Promises.

## Overview

Functions in the `promise` module bridge native libuv background work with `JsDeferred` and JavaScript `Promise` settlement.

## Usage

```ts
import { processBuffer, processBufferOnPool } from "repository";

const buffer = Buffer.from("hello");
const result = await processBuffer(buffer);
```

## API Reference

- `asyncPlus100(promise: Promise<number>): Promise<number>`
- `processBuffer(buffer: Buffer): Promise<Buffer>`
- `processBufferOnPool(buffer: Buffer): Promise<Buffer>`
