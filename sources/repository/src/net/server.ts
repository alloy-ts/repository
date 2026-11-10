import { File } from "../fs/file.ts";
import { Request, Response } from "./request.ts";
import { RegExpRouter, SmartRouter, TrieRouter, type Router } from "./router.ts";
import * as native from "../../../../dist/index.js";

export { RegExpRouter, SmartRouter, TrieRouter };
export type { Router };

type RoutedRequest = Request & { params: Record<string, string> };
type LegacyHandler = (request: RoutedRequest) => Response | Promise<Response>;
type LegacyRoute = Response | File | LegacyHandler | Partial<Record<string, LegacyHandler | Response | File>>;

/** Fetch handler mounted under a path. A Server is accepted here as well. */
export type MountHandler = (
  request: globalThis.Request,
  env?: unknown,
  executionCtx?: unknown,
) => Response | Promise<Response>;

export interface MountOptions {
  replaceRequest?: ((request: globalThis.Request) => globalThis.Request) | false;
}

/** Hono-style environment. `Bindings` is `c.env`; `Variables` types `c.set` / `c.get`. */
export interface ServerEnv {
  Bindings?: unknown;
  Variables?: object;
}

export interface EmptyEnv {
  Bindings: unknown;
  Variables: Record<string, unknown>;
}

export type BindingsOf<E> = E extends { Bindings: infer Bindings } ? Bindings : unknown;
export type VariablesOf<E> = E extends { Variables: infer Variables extends object }
  ? Variables
  : Record<string, unknown>;

export interface ServerOptions<E extends ServerEnv = EmptyEnv> {
  /** When true (the default), `/hello` and `/hello/` are different routes. */
  strict?: boolean;
  /** Router implementation. The default is SmartRouter (regexp, then trie). */
  router?: Router<Handler<E>> | Router<unknown>;
  /**
   * Routing path for a request. Ignored when `strict` is false.
   * The default is the URL pathname without its query or fragment.
   */
  getPath?: (request: globalThis.Request, options?: { env?: BindingsOf<E> }) => string;
  port?: number;
  host?: string;
  routes?: Record<string, LegacyRoute>;
  fetch?: LegacyHandler;
}

export type Next = () => Promise<void>;
export type Handler<E extends ServerEnv = EmptyEnv> = (
  c: Context<E>,
  next: Next,
) => Response | Promise<Response | void> | void | Promise<void>;
export type ErrorHandler<E extends ServerEnv = EmptyEnv> = (
  err: Error,
  c: Context<E>,
) => Response | Promise<Response>;
export type NotFoundHandler<E extends ServerEnv = EmptyEnv> = (
  c: Context<E>,
) => Response | Promise<Response>;

interface RouteRecord<E extends ServerEnv = EmptyEnv> {
  method: string;
  path: string;
  basePath: string;
  handler: Handler<E>;
}

interface RocketListener {
  readonly port: number;
  readonly listening: boolean;
  start(): Promise<number>;
  stop(): Promise<void>;
}

interface Core<E extends ServerEnv = EmptyEnv> {
  host: string;
  configuredPort: number;
  boundPort?: number;
  rocket?: RocketListener;
  /** Referenced timer so a listening server keeps the Node process alive. */
  keepAlive?: ReturnType<typeof setInterval>;
  router: Router<Handler<E>>;
  routes: RouteRecord<E>[];
  notFound: NotFoundHandler<E>;
  notFoundCustom: boolean;
  onError: ErrorHandler<E>;
  errorCustom: boolean;
  getPath: (request: globalThis.Request, options?: { env?: BindingsOf<E> }) => string;
  legacyRoutes: [string, LegacyRoute][];
  legacyFetch?: LegacyHandler;
}

function defaultNotFound<E extends ServerEnv>(c: Context<E>): Response {
  return c.text("404 Not Found", 404);
}
function defaultOnError<E extends ServerEnv>(err: Error, c: Context<E>): Response {
  console.error(`${err}`);
  return c.text("Internal Server Error", 500);
}

/** Request view passed to handlers as `c.req`. */
export class HonoRequest {
  readonly raw: globalThis.Request;
  #params: Record<string, string>;

  constructor(raw: globalThis.Request, params: Record<string, string>) {
    this.raw = raw;
    this.#params = params;
  }

  get method(): string {
    return this.raw.method;
  }

  get url(): string {
    return this.raw.url;
  }

  get path(): string {
    return defaultGetPath(this.raw);
  }

  header(name: string): string | undefined;
  header(): Record<string, string>;
  header(name?: string): string | undefined | Record<string, string> {
    if (name === undefined) {
      const all: Record<string, string> = {};
      this.raw.headers.forEach((value, key) => {
        all[key] = value;
      });
      return all;
    }
    return this.raw.headers.get(name) ?? undefined;
  }

  /** One named param, or every param captured by the route currently running. */
  param(name: string): string | undefined;
  param(): Record<string, string>;
  param(name?: string): string | undefined | Record<string, string> {
    if (name === undefined) return { ...this.#params };
    return this.#params[name];
  }

  query(name: string): string | undefined;
  query(): Record<string, string>;
  query(name?: string): string | undefined | Record<string, string> {
    const params = new URL(this.raw.url).searchParams;
    if (name === undefined) {
      const all: Record<string, string> = {};
      params.forEach((value, key) => {
        all[key] ??= value;
      });
      return all;
    }
    return params.get(name) ?? undefined;
  }

  json<T = unknown>(): Promise<T> {
    return this.raw.json() as Promise<T>;
  }

  text(): Promise<string> {
    return this.raw.text();
  }

  arrayBuffer(): Promise<ArrayBuffer> {
    return this.raw.arrayBuffer();
  }

  blob(): Promise<Blob> {
    return this.raw.blob();
  }

  formData(): Promise<FormData> {
    return this.raw.formData();
  }

  /** @internal Route dispatch updates the captures before each handler. */
  assignParams(params: Record<string, string>): void {
    this.#params = params;
  }
}

/** Handler context. `c.text` / `c.json` build a response; `c.set` / `c.get` share request state. */
export class Context<E extends ServerEnv = EmptyEnv> {
  readonly req: HonoRequest;
  readonly env: BindingsOf<E>;
  readonly executionCtx: unknown;
  #headers = new Headers();
  #wroteHeaders = false;
  #status = 200;
  #vars = new Map<string, unknown>();
  #res?: Response;
  #finalized = false;
  readonly #notFound: NotFoundHandler<E>;

  constructor(
    request: globalThis.Request,
    env: BindingsOf<E> | undefined,
    executionCtx: unknown,
    params: Record<string, string>,
    notFound: NotFoundHandler<E>,
  ) {
    this.req = new HonoRequest(request, params);
    this.env = env as BindingsOf<E>;
    this.executionCtx = executionCtx;
    this.#notFound = notFound;
  }

  get finalized(): boolean {
    return this.#finalized;
  }

  get res(): Response {
    if (!this.#res) throw new Error("Context is not finalized");
    return this.#res;
  }

  header(name: string, value: string, options?: { append?: boolean }): this {
    this.#wroteHeaders = true;
    if (options?.append) this.#headers.append(name, value);
    else this.#headers.set(name, value);
    if (this.#res && this.#res.type !== "error" && this.#res.status !== 0) {
      this.#res = copyResponse(this.#res, (headers) => {
        if (options?.append) headers.append(name, value);
        else headers.set(name, value);
      });
    }
    return this;
  }

  /** Remember `code`, and update a response that has already been produced. */
  status(code: number): this {
    this.#status = code;
    if (this.#res && this.#res.type !== "error" && this.#res.status !== 0) {
      const current = this.#res;
      this.#res = new Response(current.body, {
        status: code,
        statusText: current.statusText,
        headers: current.headers,
      });
    }
    return this;
  }

  text(body: string, status = this.#status): Response {
    return this.#build(body, status, "text/plain; charset=UTF-8");
  }

  json(body: unknown, status = this.#status): Response {
    return this.#build(JSON.stringify(body), status, "application/json");
  }

  html(html: string, status = this.#status): Response {
    return this.#build(html, status, "text/html; charset=UTF-8");
  }

  body(
    data: ConstructorParameters<typeof Response>[0],
    status = this.#status,
    headers?: ConstructorParameters<typeof globalThis.Headers>[0],
  ): Response {
    const merged = new Headers(this.#headers);
    if (headers) {
      new Headers(headers).forEach((value, key) => {
        merged.set(key, value);
      });
    }
    return new Response(data, { status, headers: merged });
  }

  redirect(url: string, status = 302): Response {
    const redirected = Response.redirect(url, status);
    if ([...this.#headers].length === 0) return redirected;
    return copyResponse(redirected, (headers) => {
      for (const [key, value] of this.#headers) {
        if (key.toLowerCase() !== "location") headers.set(key, value);
      }
    });
  }

  /** Run the application's `notFound` handler, including one registered with `server.notFound`. */
  notFound(): Promise<Response> {
    return Promise.resolve(this.#notFound(this));
  }

  set<Key extends keyof VariablesOf<E> & string>(key: Key, value: VariablesOf<E>[Key]): void {
    this.#vars.set(key, value);
  }

  get<Key extends keyof VariablesOf<E> & string>(key: Key): VariablesOf<E>[Key] {
    return this.#vars.get(key) as VariablesOf<E>[Key];
  }

  /**
   * Install `response` even when a downstream handler already finalized one.
   * `onError` uses this so a throw after `await next()` can replace that response.
   */
  replace(response: Response): void {
    this.#finalized = false;
    this.#res = undefined;
    this.finalize(response);
  }

  /** @internal */
  finalize(response: Response): void {
    if (this.#finalized) return;
    if (!this.#wroteHeaders || response.type === "error" || response.status === 0) {
      this.#res = response;
    } else {
      this.#res = copyResponse(response, (headers) => {
        for (const [key, value] of this.#headers) headers.set(key, value);
      });
    }
    this.#finalized = true;
  }

  #build(body: string, status: number, contentType: string): Response {
    const headers = new Headers(this.#headers);
    if (!headers.has("content-type")) headers.set("content-type", contentType);
    return new Response(body, { status, headers });
  }
}

/**
 * HTTP server with Hono-style routing.
 *
 * `fetch` and `request` dispatch in-process. They never open a socket.
 * `start` serves that same dispatcher through Rocket: the native listener
 * reads the HTTP request and the callback calls `fetch()`.
 *
 * Rocket 0.5.2 cannot represent QUERY, PURGE, or other methods outside its
 * `Method` enum. Those routes still run for `fetch()` and `request()`.
 * A live listener answers them with 400 before `fetch()` is called.
 */
export class Server<E extends ServerEnv = EmptyEnv> {
  readonly host: string;
  #core: Core<E>;
  #basePath: string;
  #path = "/";

  constructor(options?: ServerOptions<E>);
  constructor(options: ServerOptions<E> | undefined, child: { core: Core<E>; basePath: string });
  constructor(options: ServerOptions<E> = {}, child?: { core: Core<E>; basePath: string }) {
    if (child) {
      this.#core = child.core;
      this.#basePath = child.basePath;
      this.host = child.core.host;
      return;
    }
    const host = options.host ?? "127.0.0.1";
    const strict = options.strict !== false;
    this.host = host;
    this.#basePath = "/";
    this.#core = {
      host,
      configuredPort: options.port ?? 3000,
      router: (options.router as Router<Handler<E>> | undefined) ?? new SmartRouter<Handler<E>>(),
      routes: [],
      notFound: defaultNotFound,
      notFoundCustom: false,
      onError: defaultOnError,
      errorCustom: false,
      getPath: strict ? (options.getPath ?? defaultGetPath) : getPathNoStrict,
      legacyRoutes: Object.entries(options.routes ?? {}).sort(([a], [b]) => score(b) - score(a)),
      legacyFetch: options.fetch,
    };
  }

  get port(): number {
    return this.#core.boundPort ?? this.#core.configuredPort;
  }

  get listening(): boolean {
    return this.#core.rocket?.listening ?? false;
  }

  get(path: string, ...handlers: Handler<E>[]): this;
  get(handler: Handler<E>, ...handlers: Handler<E>[]): this;
  get(pathOrHandler: string | Handler<E>, ...handlers: Handler<E>[]): this {
    return this.#register("GET", pathOrHandler, handlers);
  }

  post(path: string, ...handlers: Handler<E>[]): this;
  post(handler: Handler<E>, ...handlers: Handler<E>[]): this;
  post(pathOrHandler: string | Handler<E>, ...handlers: Handler<E>[]): this {
    return this.#register("POST", pathOrHandler, handlers);
  }

  put(path: string, ...handlers: Handler<E>[]): this;
  put(handler: Handler<E>, ...handlers: Handler<E>[]): this;
  put(pathOrHandler: string | Handler<E>, ...handlers: Handler<E>[]): this {
    return this.#register("PUT", pathOrHandler, handlers);
  }

  delete(path: string, ...handlers: Handler<E>[]): this;
  delete(handler: Handler<E>, ...handlers: Handler<E>[]): this;
  delete(pathOrHandler: string | Handler<E>, ...handlers: Handler<E>[]): this {
    return this.#register("DELETE", pathOrHandler, handlers);
  }

  /**
   * Register a QUERY route. `fetch()` and `request()` dispatch it.
   * Rocket 0.5 has no QUERY variant, so `start()` answers QUERY with 400.
   */
  query(path: string, ...handlers: Handler<E>[]): this;
  query(handler: Handler<E>, ...handlers: Handler<E>[]): this;
  query(pathOrHandler: string | Handler<E>, ...handlers: Handler<E>[]): this {
    return this.#register("QUERY", pathOrHandler, handlers);
  }

  patch(path: string, ...handlers: Handler<E>[]): this;
  patch(handler: Handler<E>, ...handlers: Handler<E>[]): this;
  patch(pathOrHandler: string | Handler<E>, ...handlers: Handler<E>[]): this {
    return this.#register("PATCH", pathOrHandler, handlers);
  }

  options(path: string, ...handlers: Handler<E>[]): this;
  options(handler: Handler<E>, ...handlers: Handler<E>[]): this;
  options(pathOrHandler: string | Handler<E>, ...handlers: Handler<E>[]): this {
    return this.#register("OPTIONS", pathOrHandler, handlers);
  }

  head(path: string, ...handlers: Handler<E>[]): this;
  head(handler: Handler<E>, ...handlers: Handler<E>[]): this;
  head(pathOrHandler: string | Handler<E>, ...handlers: Handler<E>[]): this {
    return this.#register("HEAD", pathOrHandler, handlers);
  }

  all(path: string, ...handlers: Handler<E>[]): this;
  all(handler: Handler<E>, ...handlers: Handler<E>[]): this;
  all(pathOrHandler: string | Handler<E>, ...handlers: Handler<E>[]): this {
    return this.#register("ALL", pathOrHandler, handlers);
  }

  /**
   * Register one or more methods. Methods Rocket 0.5 does not know, including
   * PURGE, are dispatched by `fetch()` and `request()` only. The listener
   * answers them with 400.
   */
  on(method: string | string[], path: string | string[], ...handlers: Handler<E>[]): this {
    const methods = Array.isArray(method) ? method : [method];
    const paths = Array.isArray(path) ? path : [path];
    for (const routePath of paths) {
      this.#path = routePath;
      for (const routeMethod of methods) {
        for (const handler of handlers) this.#addRoute(routeMethod.toUpperCase(), routePath, handler);
      }
    }
    return this;
  }

  use(handler: Handler<E>, ...handlers: Handler<E>[]): this;
  use(path: string, handler: Handler<E>, ...handlers: Handler<E>[]): this;
  use(pathOrHandler: string | Handler<E>, ...handlers: Handler<E>[]): this {
    if (typeof pathOrHandler === "string") {
      this.#path = pathOrHandler;
      for (const handler of handlers) this.#addRoute("ALL", pathOrHandler, handler);
    } else {
      this.#path = "*";
      for (const handler of [pathOrHandler, ...handlers]) this.#addRoute("ALL", "*", handler);
    }
    return this;
  }

  /**
   * Copy `app`'s routes, as currently registered, under `path`.
   * Later registrations on `app` are not visible here. Not-found handlers are not copied;
   * a routed app's error handler takes priority over this server's handler.
   */
  route<Child extends ServerEnv>(path: string, app?: Server<Child>): this {
    if (!app) return this;
    const sub = this.basePath(path);
    const onError = app.#core.errorCustom ? app.#core.onError : undefined;
    for (const record of app.#core.routes) {
      if (!belongsTo(record.basePath, app.#basePath)) continue;
      const handler = record.handler;
      const wrapped = (
        onError
          ? async (c: Context<E>, next: Next) => {
              try {
                return await (handler as unknown as Handler<E>)(c, next);
              } catch (error) {
                if (error instanceof Error) return onError(error, c as unknown as Context<Child>);
                throw error;
              }
            }
          : handler
      ) as unknown as Handler<E>;
      sub.#addRoute(record.method, record.path, wrapped);
    }
    return this;
  }

  /** Return a view of this server that prefixes later routes with `path`. */
  basePath(path: string): Server<E> {
    return new Server(undefined, { core: this.#core, basePath: mergePath(this.#basePath, path) });
  }

  notFound(handler: NotFoundHandler<E>): this {
    this.#core.notFound = handler;
    this.#core.notFoundCustom = true;
    return this;
  }

  onError(handler: ErrorHandler<E>): this {
    this.#core.onError = handler;
    this.#core.errorCustom = true;
    return this;
  }

  /**
   * Forward matching requests to another server or fetch function.
   * The mounted handler sees the request path with `path` removed.
   */
  mount(
    path: string,
    application: MountHandler | Pick<Server<E>, "fetch">,
    options?: MountOptions,
  ): this {
    const prefix = mergePath(this.#basePath, path).replace(/\/$/, "") || "/";
    const handler: Handler<E> = async (c) => {
      const request = mountedRequest(c.req.raw, prefix, options);
      if (typeof application === "function") return application(request, c.env, c.executionCtx);
      return application.fetch(request, c.env, c.executionCtx);
    };
    this.#addRoute("ALL", mergePath(path, "*"), handler);
    return this;
  }

  /**
   * @deprecated Prefer `server.fetch`. Registers a global `fetch` listener.
   * Node has no service-worker `addEventListener`, so this installs one for `fetch`
   * events when the host does not already provide it.
   */
  fire(): void {
    console.warn("server.fire() is deprecated. Dispatch requests with server.fetch().");
    ensureFetchHost();
    const target = globalThis as typeof globalThis & {
      addEventListener: (
        type: string,
        listener: (event: {
          request: globalThis.Request;
          respondWith: (response: Response | Promise<Response>) => void;
        }) => void,
      ) => void;
    };
    target.addEventListener("fetch", (event) => {
      event.respondWith(this.fetch(event.request));
    });
  }

  /**
   * Dispatch one request in this process. This is the application entry point.
   * It never opens a socket and never performs a network fetch, whether or not
   * `start()` is listening.
   *
   * `HEAD` uses a HEAD handler when one matches. Otherwise it runs the GET handler
   * and still returns a response with no body. In both cases the body is omitted
   * and `content-length` describes the handler's body.
   */
  async fetch(
    request: globalThis.Request,
    env?: BindingsOf<E>,
    executionCtx?: unknown,
  ): Promise<Response> {
    if (request.method === "HEAD") {
      const path = this.#core.getPath(request, { env });
      const specific = this.#core.router.match("HEAD", path).some((route) => route.method === "HEAD");
      const response = await this.#dispatch(request, env, executionCtx, specific ? "HEAD" : "GET");
      return emptyHead(response);
    }
    return this.#dispatch(request, env, executionCtx, request.method.toUpperCase());
  }

  /**
   * In-process test helper. A string or URL becomes a Request aimed at
   * `http://localhost` and is passed to `fetch()`. An absolute URL is only the
   * request URL used for routing. This method does not contact the network and
   * does not send the request to the socket opened by `start()`.
   */
  request(
    input: globalThis.Request | string | URL,
    requestInit?: RequestInit,
    env?: BindingsOf<E>,
    executionCtx?: unknown,
  ): Promise<Response> {
    if (input instanceof globalThis.Request) {
      return this.fetch(requestInit ? new Request(input, requestInit) : input, env, executionCtx);
    }
    const value = input.toString();
    const url = /^https?:\/\//.test(value) ? value : `http://localhost${mergePath("/", value)}`;
    return this.fetch(new Request(url, requestInit), env, executionCtx);
  }

  async start(): Promise<number> {
    if (this.#core.rocket) throw new Error("server already started");
    const rocket = new native.RocketServer(this.#core.configuredPort, this.host, (raw: string) =>
      dispatchRocket(this, raw),
    );
    const port = await rocket.start();
    this.#core.boundPort = port;
    this.#core.rocket = rocket;
    // Rocket's runtime does not register a libuv handle. This timer does.
    this.#core.keepAlive ??= setInterval(() => {}, 2 ** 30);
    return port;
  }

  serve(): Promise<number> {
    return this.start();
  }

  async stop(): Promise<void> {
    const rocket = this.#core.rocket;
    if (!rocket) return;
    this.#core.rocket = undefined;
    if (this.#core.keepAlive) clearInterval(this.#core.keepAlive);
    this.#core.keepAlive = undefined;
    await rocket.stop();
  }

  #register(method: string, pathOrHandler: string | Handler<E>, handlers: Handler<E>[]): this {
    const queue = [...handlers];
    if (typeof pathOrHandler === "string") this.#path = pathOrHandler;
    else queue.unshift(pathOrHandler);
    for (const handler of queue) this.#addRoute(method, this.#path, handler);
    return this;
  }

  #addRoute(method: string, path: string, handler: Handler<E>): void {
    const full = mergePath(this.#basePath, path);
    this.#core.routes.push({ method, path: full, basePath: this.#basePath, handler });
    this.#core.router.add(method, full, handler);
  }

  async #dispatch(
    request: globalThis.Request,
    env: BindingsOf<E> | undefined,
    executionCtx: unknown,
    method: string,
  ): Promise<Response> {
    const path = this.#core.getPath(request, { env });
    const matched = this.#core.router.match(method, path);
    if (matched.length > 0) {
      const context = new Context<E>(request, env, executionCtx, matched[0]?.params ?? {}, this.#core.notFound);
      return compose(matched, context, this.#core.notFound, this.#core.onError);
    }
    const legacy = await this.#legacy(request);
    if (legacy) return legacy;
    if (this.#core.routes.length > 0 || this.#core.notFoundCustom) {
      const context = new Context<E>(request, env, executionCtx, {}, this.#core.notFound);
      return compose([], context, this.#core.notFound, this.#core.onError);
    }
    return new Response("Not Found", { status: 404 });
  }

  async #legacy(request: globalThis.Request): Promise<Response | undefined> {
    const pathname = new URL(request.url).pathname;
    const method = request.method;
    let selected: LegacyRoute | undefined;
    let params: Record<string, string> | undefined;
    let matched = false;
    for (const [pattern, route] of this.#core.legacyRoutes) {
      const found = matchLegacy(pattern, pathname);
      if (found) {
        matched = true;
        params = found;
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
      selected = selected[method] ?? (method === "HEAD" ? selected["GET"] : undefined);
    }
    const routed = request as RoutedRequest;
    routed.params = params ?? {};
    if (selected instanceof File) {
      return new Response(await selected.bytes(), { headers: { "content-type": selected.type } });
    }
    if (typeof selected === "function") {
      const response = await selected(routed);
      if (!(response instanceof globalThis.Response)) throw new TypeError("route must return a Response");
      return response;
    }
    if (selected instanceof globalThis.Response) return selected.clone();
    if (this.#core.legacyFetch) {
      const response = await this.#core.legacyFetch(routed);
      if (!(response instanceof globalThis.Response)) throw new TypeError("route must return a Response");
      return response;
    }
    if (matched) return new Response("Not Found", { status: 404 });
    return undefined;
  }

}

interface RocketMessage {
  port: number;
  method: string;
  path: string;
  headers: [string, string][];
  body: string;
}

/** Rocket calls this once per accepted request. The result is written by Rocket. */
async function dispatchRocket<E extends ServerEnv>(server: Server<E>, raw: string): Promise<string> {
  try {
    const message = JSON.parse(raw) as RocketMessage;
    const host = server.host.includes(":") ? `[${server.host}]` : server.host;
    const target = message.path.startsWith("/") ? message.path : `/${message.path}`;
    const method = message.method.toUpperCase();
    const payload = message.body ? Buffer.from(message.body, "base64") : undefined;
    const request = new Request(`http://${host}:${message.port}${target}`, {
      method,
      headers: message.headers,
      body: method === "GET" || method === "HEAD" || !payload?.byteLength ? undefined : payload,
    });
    const result = await server.fetch(request);
    const body = Buffer.from(await result.arrayBuffer()).toString("base64");
    return JSON.stringify({
      status: result.status || 500,
      headers: [...result.headers.entries()],
      body,
    });
  } catch {
    return JSON.stringify({
      status: 500,
      headers: [["content-type", "text/plain;charset=UTF-8"]],
      body: Buffer.from("Internal Server Error").toString("base64"),
    });
  }
}

async function emptyHead(response: Response): Promise<Response> {
  const headers = new Headers(response.headers);
  if (!headers.has("content-length")) {
    const bytes = await response.arrayBuffer();
    headers.set("content-length", String(bytes.byteLength));
  }
  return new Response(null, { status: response.status, statusText: response.statusText, headers });
}

type FetchListener = (event: {
  type: string;
  request: globalThis.Request;
  respondWith: (response: Response | Promise<Response>) => void;
}) => void;

const fetchListeners = new Set<FetchListener>();
let fetchHostReady = false;

function ensureFetchHost(): void {
  const target = globalThis as typeof globalThis & {
    addEventListener?: (type: string, listener: FetchListener) => void;
    removeEventListener?: (type: string, listener: FetchListener) => void;
    dispatchEvent?: (event: { type: string }) => boolean;
  };
  if (typeof target.addEventListener === "function") return;
  if (fetchHostReady) return;
  fetchHostReady = true;
  target.addEventListener = (type, listener) => {
    if (type === "fetch") fetchListeners.add(listener);
  };
  target.removeEventListener = (type, listener) => {
    if (type === "fetch") fetchListeners.delete(listener);
  };
  target.dispatchEvent = (event) => {
    if (event.type !== "fetch") return false;
    for (const listener of Array.from(fetchListeners)) listener(event as Parameters<FetchListener>[0]);
    return true;
  };
}

async function compose<E extends ServerEnv>(
  routes: { handler: Handler<E>; params: Record<string, string> }[],
  context: Context<E>,
  notFound: NotFoundHandler<E>,
  onError: ErrorHandler<E>,
): Promise<Response> {
  let index = -1;
  const run = async (position: number): Promise<void> => {
    if (position <= index) throw new Error("next() called multiple times");
    index = position;
    const route = routes[position];
    let result: unknown;
    try {
      if (route) {
        let calledNext = false;
        context.req.assignParams(route.params);
        result = await route.handler(context, async () => {
          calledNext = true;
          await run(position + 1);
        });
        if (result instanceof globalThis.Response && !context.finalized) context.finalize(result);
        // A handler that neither returns a response nor continues the chain is
        // not a 404. Do not call the next handler automatically.
        if (!context.finalized && !calledNext) {
          throw new Error("Context is not finalized. Return a Response or await next().");
        }
      } else {
        result = await notFound(context);
        if (result instanceof globalThis.Response && !context.finalized) context.finalize(result);
      }
    } catch (error) {
      if (!(error instanceof Error)) throw error;
      const recovered = await onError(error, context);
      if (recovered instanceof globalThis.Response) context.replace(recovered);
      else if (!context.finalized) {
        context.finalize(new Response("Internal Server Error", { status: 500 }));
      }
      return;
    }
  };
  await run(0);
  if (!context.finalized) context.finalize(await notFound(context));
  return context.res;
}

function copyResponse(response: Response, edit: (headers: Headers) => void): Response {
  const headers = new Headers(response.headers);
  edit(headers);
  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers,
  });
}

function mountedRequest(
  request: globalThis.Request,
  prefix: string,
  options?: MountOptions,
): globalThis.Request {
  if (options?.replaceRequest === false) return request;
  if (typeof options?.replaceRequest === "function") return options.replaceRequest(request);
  const url = new URL(request.url);
  if (url.pathname === prefix) url.pathname = "/";
  else if (prefix !== "/" && url.pathname.startsWith(`${prefix}/`)) {
    url.pathname = url.pathname.slice(prefix.length) || "/";
  }
  const clone = request.clone();
  return new Request(url, clone);
}

function belongsTo(recordBase: string, appBase: string): boolean {
  if (appBase === "/") return true;
  return recordBase === appBase || recordBase.startsWith(`${appBase}/`);
}

function mergePath(base: string, sub: string): string {
  const left = base.startsWith("/") ? base : `/${base}`;
  if (sub === "/" || sub === "") return left;
  const trimmed = left.endsWith("/") ? left.slice(0, -1) : left;
  const right = sub.startsWith("/") ? sub : `/${sub}`;
  return `${trimmed}${right}` || "/";
}

function defaultGetPath(request: globalThis.Request): string {
  const url = request.url;
  const protocol = url.indexOf("://");
  const start = url.indexOf("/", protocol === -1 ? 0 : protocol + 3);
  if (start < 0) return "/";
  let end = url.length;
  const query = url.indexOf("?", start);
  const hash = url.indexOf("#", start);
  if (query !== -1) end = Math.min(end, query);
  if (hash !== -1) end = Math.min(end, hash);
  return url.slice(start, end) || "/";
}

function getPathNoStrict(request: globalThis.Request): string {
  const path = defaultGetPath(request);
  return path.length > 1 && path.endsWith("/") ? path.slice(0, -1) : path;
}

function score(pattern: string): number {
  return pattern
    .split("/")
    .reduce((total, part) => total + (part === "*" ? -1000 : part.startsWith(":") ? 1 : 10), 0);
}

function matchLegacy(pattern: string, path: string): Record<string, string> | undefined {
  const parts = pattern.split("/");
  const actual = path.split("/");
  const params: Record<string, string> = {};
  for (let index = 0; index < parts.length; index++) {
    const part = parts[index]!;
    const segment = actual[index];
    if (part === "*") return params;
    if (segment === undefined) return;
    if (part.startsWith(":")) params[part.slice(1)] = decodeURIComponent(segment);
    else if (part !== segment) return;
  }
  return parts.length === actual.length ? params : undefined;
}
