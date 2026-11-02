# Thread API

Exposes OS threading and `libuv` thread pool handles to JavaScript, mirroring `std::thread` and libuv thread pool inspection.

## Classes

### `Thread`

Wraps [`std::thread::Thread`].

#### Methods

- `static current(): Thread`: Returns a handle to the currently running thread.
- `id(): number`: Unique numeric identifier for the thread.
- `name(): string | null`: Name of the thread if configured.
- `static sleep(millis: number): void`: Suspends the calling thread.
- `static yieldNow(): void`: Cooperatively yields execution.
- `static park(): void`: Blocks thread until unparked.
- `static parkTimeout(millis: number): void`: Blocks with a timeout.

### `Builder`

Thread factory mirroring [`std::thread::Builder`].

#### Methods

- `constructor()`: Initializes thread configuration.
- `name(name: string): Builder`: Names the thread to be spawned.
- `stackSize(bytes: number): Builder`: Configures stack size.
- `spawn(task: () => void): JoinHandle`: Spawns the thread to execute the callback.

### `JoinHandle`

An owned permission to join or await a spawned OS thread.

#### Methods

- `thread(): Thread`: Retrieves the underlying `Thread` handle.
- `join(): void`: Blocks the caller until the spawned thread exits.
- `joinAsync(done: () => void): void`: Asynchronously awaits thread completion on a helper thread.

### `ThreadPool`

Inspects and drives the global `libuv` worker thread pool.

#### Methods

- `constructor()`: Initializes handle against default loop.
- `static maxThreads(): number`: Maximum threads allowed in the libuv thread pool.
- `static defaultThreads(): number`: Default thread count when `UV_THREADPOOL_SIZE` is unset.
- `static effectiveThreads(): number`: Actual thread pool size in the process.
