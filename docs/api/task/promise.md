# Promise API

The `promise` module provides zero-overhead interoperability between Rust async futures / background tasks and JavaScript Promises via NAPI-RS.

## Overview

Facilitates bidirectional Promise creation, resolution, and async worker pool offloading:
- Wrapping Rust deferred operations as JavaScript Promises.
- Bridging Node.js libuv event loop with background worker threads.
- Non-blocking task execution and error propagation.

## API Reference

### Functions

- `asyncPlus100(promise: Promise<number>): Promise<number>`
- `processBuffer(buffer: Buffer): Promise<Buffer>`
- `processBufferOnPool(buffer: Buffer): Promise<any>`
