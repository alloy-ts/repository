# Promise API

The `promise` module provides asynchronous bridge utilities between Rust tasks, libuv event loops, and JavaScript Promises.

## Overview

Facilitates resolution, rejection, timeouts, and batching of promises across the FFI boundary without thread starvation or unhandled rejections.

## API Reference

### Functions

- `delayPromise(ms: number): Promise<void>`: Resolves after the specified duration.
- `resolveValue<T>(value: T): Promise<T>`: Returns an immediately resolved promise.
- `rejectError(error: Error): Promise<never>`: Returns an immediately rejected promise.
