# Promise API

Asynchronous task bridging and deferred promise resolution utilities connecting Node.js Promises with Rust thread pool execution.

## Functions

### `asyncPlus100(promise: Promise<number>): Promise<number>`

Awaits an incoming JS promise and transforms the result asynchronously.

### `promiseCallback(promise: Promise<number>): Promise<number>`

Consumes a promise and resolves with the modified value using raw NAPI promise settlement.

### `processBuffer(buffer: Buffer): Promise<Buffer>`

Processes and echoes a Node `Buffer` asynchronously using `AsyncBlock`.

### `processBufferOnPool(buffer: Buffer): Promise<Buffer>`

Dispatches buffer processing onto the global `libuv` worker thread pool and settles a JS Promise upon return to the event loop.
