import { RocketServer } from "../../../../dist/index.js";
import { File } from "../fs/file.ts";
import { Request, Response } from "./request.ts";

type RoutedRequest = Request & { params: Record<string, string> };
type Handler = (request: RoutedRequest) => Response | Promise<Response>;
type Route = Response | File | Handler | Partial<Record<string, Handler | Response | File>>;
export interface ServerOptions {
  port?: number;
  host?: string;
  routes?: Record<string, Route>;
  fetch?: Handler;
}
export class Server {
  readonly #native: RocketServer;
  readonly host: string;
  #keepAlive?: ReturnType<typeof setInterval>;
  constructor(options: ServerOptions = {}) {
    this.host = options.host ?? "127.0.0.1";
    const host = this.host;
    const routes = Object.entries(options.routes ?? {}).sort(([a], [b]) => score(b) - score(a));
    this.#native = new RocketServer(options.port ?? 3000, this.host, async (input: string) => {
      try {
        const wire = JSON.parse(input) as {
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
        ) as RoutedRequest;
        request.params = {};
        let selected: Route | undefined;
        for (const [pattern, route] of routes) {
          const params = match(pattern, new URL(request.url).pathname);
          if (params) {
            request.params = params;
            selected = route;
            break;
          }
        }
        if (
          selected &&
          !(selected instanceof globalThis.Response) &&
          !(selected instanceof File) &&
          typeof selected !== "function"
        ) {
          selected =
            selected[wire.method] ?? (wire.method === "HEAD" ? selected["GET"] : undefined);
        }
        let response =
          selected instanceof File
            ? new Response(await selected.bytes(), { headers: { "content-type": selected.type } })
            : typeof selected === "function"
              ? await selected(request)
              : selected instanceof globalThis.Response
                ? selected.clone()
                : options.fetch
                  ? await options.fetch(request)
                  : new Response("Not Found", { status: 404 });
        if (!(response instanceof globalThis.Response))
          throw new TypeError("route must return a Response");
        // File extends Blob with a lazy disk body, so routes accept it explicitly.
        return JSON.stringify({
          status: response.status,
          headers: [...response.headers],
          body: Buffer.from(await response.arrayBuffer()).toString("base64"),
        });
      } catch {
        return JSON.stringify({
          status: 500,
          headers: [],
          body: Buffer.from("Internal Server Error").toString("base64"),
        });
      }
    });
  }
  get port(): number {
    return this.#native.port;
  }
  get listening(): boolean {
    return this.#native.listening;
  }
  async start(): Promise<number> {
    if (this.#keepAlive) throw new Error("server already started");
    let started = false;
    // Tokio's listener and pending start future are not libuv handles.
    this.#keepAlive = setInterval(() => {
      if (started && !this.listening) clearInterval(this.#keepAlive);
    }, 1000);
    try {
      const port = await this.#native.start();
      started = true;
      return port;
    } catch (error) {
      clearInterval(this.#keepAlive);
      this.#keepAlive = undefined;
      throw error;
    }
  }
  serve(): Promise<number> {
    return this.start();
  }
  async stop(): Promise<void> {
    try {
      await this.#native.stop();
    } finally {
      clearInterval(this.#keepAlive);
      this.#keepAlive = undefined;
    }
  }
}
function score(pattern: string): number {
  return pattern
    .split("/")
    .reduce((n, part) => n + (part === "*" ? -1000 : part.startsWith(":") ? 1 : 10), 0);
}
function match(pattern: string, path: string): Record<string, string> | undefined {
  const parts = pattern.split("/");
  const actual = path.split("/");
  const params: Record<string, string> = {};
  for (let i = 0; i < parts.length; i++) {
    const part = parts[i]!;
    const segment = actual[i];
    if (part === "*") return params;
    if (segment === undefined) return;
    if (part.startsWith(":")) params[part.slice(1)] = decodeURIComponent(segment);
    else if (part !== segment) return;
  }
  return parts.length === actual.length ? params : undefined;
}
