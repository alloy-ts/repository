import { NativeServer } from "../../../../dist/index.js";
import { Request } from "./request.ts";
import { createApp, type AppOptions } from "./app.ts";

export interface ServerOptions extends AppOptions<Request> {
  port?: number;
  host?: string;
  strict?: boolean;
}
export class Server {
  readonly #native: NativeServer;
  readonly host: string;
  #keepAlive?: ReturnType<typeof setInterval>;
  #cancelResponses: () => void;
  constructor(options: ServerOptions = {}) {
    this.host = options.host ?? "127.0.0.1";
    const host = this.host;
    let app: ReturnType<typeof createApp<Request>>;
    const readers = new Map<number, ReadableStreamDefaultReader<Uint8Array>>();
    this.#cancelResponses = () => {
      for (const reader of readers.values()) {
        void reader
          .cancel()
          .catch(() => {})
          .finally(() => reader.releaseLock());
      }
      readers.clear();
    };
    let nextStream = 0;
    this.#native = new NativeServer(
      options.port ?? 3000,
      this.host,
      async (input: string) => {
        const message = JSON.parse(input);
        if (message.op === "cancel") {
          const reader = readers.get(message.id);
          readers.delete(message.id);
          if (reader) {
            try {
              await reader.cancel();
            } finally {
              reader.releaseLock();
            }
          }
          return "";
        }
        if (message.op === "read") {
          const reader = readers.get(message.id);
          if (!reader) return "";
          try {
            for (;;) {
              const { value, done } = await reader.read();
              if (done) {
                readers.delete(message.id);
                reader.releaseLock();
                return "";
              }
              if (value.byteLength) return Buffer.from(value).toString("base64");
            }
          } catch (error) {
            readers.delete(message.id);
            reader.releaseLock();
            throw error;
          }
        }
        try {
          const wire = message as {
            port: number;
            method: string;
            path: string;
            headers: [string, string][];
            body: string;
          };
          const request = new Request(
            `http://${host.includes(":") ? `[${host}]` : host}:${wire.port}${wire.path}`,
            {
              method: wire.method,
              headers: wire.headers,
              body: ["GET", "HEAD"].includes(wire.method)
                ? undefined
                : Buffer.from(wire.body, "base64"),
            },
          );
          const response = await app(request);
          const stream = response.body ? ++nextStream : undefined;
          if (stream !== undefined) readers.set(stream, response.body!.getReader());
          return JSON.stringify({
            status: response.status,
            headers: [...response.headers],
            stream,
          });
        } catch {
          return JSON.stringify({
            status: 500,
            headers: [],
            body: Buffer.from("Internal Server Error").toString("base64"),
          });
        }
      },
      options.strict ?? true,
    );
    for (const [id, [pattern, route]] of Object.entries(Object.entries(options.routes ?? {}))) {
      const methodMap =
        route &&
        !(route instanceof Response) &&
        !(route instanceof Blob) &&
        typeof route !== "function";
      if (methodMap) {
        for (const method of Object.keys(route))
          this.#native.addRoute(method, pattern, Number(id), false);
        if ("GET" in route && !("HEAD" in route))
          this.#native.addRoute("HEAD", pattern, Number(id), false);
      } else this.#native.addRoute("*", pattern, Number(id), false);
    }
    app = createApp({
      ...options,
      matchRoute: (method, path) => this.#native.matchRoute(method, path),
    });
  }
  get port(): number {
    return this.#native.port;
  }
  get listening(): boolean {
    return this.#native.listening;
  }
  async start(): Promise<number> {
    // Native start serializes concurrent calls and reaps completed listeners.
    const previousTimer = this.#keepAlive;
    let started = false;
    // Tokio's listener and pending start future are not libuv handles.
    const timer = setInterval(() => {
      if (started && !this.listening) {
        clearInterval(timer);
        if (this.#keepAlive === timer) this.#keepAlive = undefined;
      }
    }, 1000);
    try {
      const port = await this.#native.start();
      started = true;
      clearInterval(previousTimer);
      this.#keepAlive = timer;
      return port;
    } catch (error) {
      clearInterval(timer);
      throw error;
    }
  }
  serve(): Promise<number> {
    return this.start();
  }
  async stop(): Promise<void> {
    this.#cancelResponses();
    try {
      await this.#native.stop();
    } finally {
      clearInterval(this.#keepAlive);
      this.#keepAlive = undefined;
    }
  }
}
