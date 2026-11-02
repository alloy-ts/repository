# Thread API

The `thread` module provides low-level OS and worker thread management, thread identification, affinity control, and execution utilities for multithreaded operations.

## Overview

Exposes native thread primitives to JavaScript/TypeScript:
- Querying current thread properties (`isMainThread`, `threadId`).
- Starting and stopping threads.
- Thread affinity and execution inspection.

## API Reference

### Functions

- `isMainThread(): boolean`: Returns whether the current execution context is on the main process thread.
- `threadId(): number`: Returns the thread ID of the current thread.
