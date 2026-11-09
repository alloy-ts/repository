import { RocketServer } from "../../../../dist/index.js";
import { File } from "../fs/file.ts";
import { Request, Response } from "./request.ts";
import { RegExpRouter, SmartRouter, type Router } from "./router.ts";

export { RegExpRouter, SmartRouter };
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

export interface ServerOptions {
  /** When true (the default), `/hello` and `/hello/` are different routes. */
  strict?: boolean;
  /** Router implementation. The default is SmartRouter, which matches RegExpRouter. */
  router?: Router<Handler>;
  /**
   * Routing path for a request. Ignored when `strict` is false.
   * The default is the URL pathname without its query or fragment.
   */
  getPath?: (request: globalThis.Request, options?: { env?: unknown }) => string;
  port?: number;
  host?: string;
  routes?: Record<string, LegacyRoute>;
  fetch?: LegacyHandler;
}

export type Next = () => Promise<void>;
export type Handler = (c: Context, next: Next) => Response | Promise<Response | void> | void | Promise<void>;
export type ErrorHandler = (err: Error, c: Context) => Response | Promise<Response>;
export type NotFoundHandler = (c: Context) => Response | Promise<Response>;

interface RouteRecord {
  method: string;
  path: string;
  basePath: string;
  handler: Handler;
}

interface Core {
  host: string;
  router: Router<Handler>;
  routes: RouteRecord[];
  notFound: NotFoundHandler;
  notFoundCustom: boolean;
  onError: ErrorHandler;
  errorCustom: boolean;
  getPath: (request: globalThis.Request, options?: { env?: unknown }) => string;
  legacyRoutes: [string, LegacyRoute][];
  legacyFetch?: LegacyHandler;
  native: RocketServer;
}

const defaultNotFound: NotFoundHandler = (c) => c.text("404 Not Found", 404);
const defaultOnError: ErrorHandler = (err, c) => {
  console.error(`${err}`);
  return c.text("Internal Server Error", 500);
};

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

  header(name: string): string | undefined {
    return this.raw.headers.get(name) ?? undefined;
  }

  /** One named param, or every param captured by the route currently running. */
  param(name: string): string | undefined;
  param(): Record<string, string>;
  param(name?: string): string | undefined | Record<string, string> {
    if (name === undefined) return { ...this.#params };
    return this.#params[name];
  }

  query(name: string): string | undefined {
    return new URL(this.raw.url).searchParams.get(name) ?? undefined;
  }

  /** @internal Route dispatch updates the captures before each handler. */
  assignParams(params: Record<string, string>): void {
    this.#params = params;
  }
}

/** Handler context. `c.text` / `c.json` build a response; `c.set` / `c.get` share request state. */
export class Context {
  readonly req: HonoRequest;
  readonly env: unknown;
  readonly executionCtx: unknown;
  #headers = new Headers();
  #wroteHeaders = false;
  #vars = new Map<string, unknown>();
  #res?: Response;
  #finalized = false;

  constructor(
    request: globalThis.Request,
    env: unknown,
    executionCtx: unknown,
    params: Record<string, string>,
  ) {
    this.req = new HonoRequest(request, params);
    this.env = env;
    this.executionCtx = executionCtx;
  }

  get finalized(): boolean {
    return this.#finalized;
  }

  get res(): Response {
    if (!this.#res) throw new Error("Context is not finalized");
    return this.#res;
  }

  header(name: string, value: string): this {
    this.#wroteHeaders = true;
    this.#headers.set(name, value);
    if (this.#res && this.#res.type !== "error" && this.#res.status !== 0) {
      this.#res = copyResponse(this.#res, (headers) => headers.set(name, value));
    }
    return this;
  }

  text(body: string, status = 200): Response {
    return this.#build(body, status, "text/plain; charset=UTF-8");
  }

  json(body: unknown, status = 200): Response {
    return this.#build(JSON.stringify(body), status, "application/json");
  }

  set(key: string, value: unknown): void {
    this.#vars.set(key, value);
  }

  get(key: string): unknown {
    return this.#vars.get(key);
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
 * `fetch` and `request` dispatch in-process. `start` serves the same dispatcher
 * through the Rocket listener.
 */
export class Server {
  readonly host: string;
  #core: Core;
  #basePath: string;
  #path = "/";
  #keepAlive?: ReturnType<typeof setInterval>;

  constructor(options?: ServerOptions);
  constructor(options: ServerOptions | undefined, child: { core: Core; basePath: string });
  constructor(options: ServerOptions = {}, child?: { core: Core; basePath: string }) {
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
      router: options.router ?? new SmartRouter<Handler>(),
      routes: [],
      notFound: defaultNotFound,
      notFoundCustom: false,
      onError: defaultOnError,
      errorCustom: false,
      getPath: strict ? (options.getPath ?? defaultGetPath) : getPathNoStrict,
      legacyRoutes: Object.entries(options.routes ?? {}).sort(([a], [b]) => score(b) - score(a)),
      legacyFetch: options.fetch,
      native: new RocketServer(options.port ?? 3000, host, (input: string) => this.#handleWire(input)),
    };
  }

  get port(): number {
    return this.#core.native.port;
  }

  get listening(): boolean {
    return this.#core.native.listening;
  }

  get(path: string, ...handlers: Handler[]): this;
  get(handler: Handler, ...handlers: Handler[]): this;
  get(pathOrHandler: string | Handler, ...handlers: Handler[]): this {
    return this.#register("GET", pathOrHandler, handlers);
  }

  post(path: string, ...handlers: Handler[]): this;
  post(handler: Handler, ...handlers: Handler[]): this;
  post(pathOrHandler: string | Handler, ...handlers: Handler[]): this {
    return this.#register("POST", pathOrHandler, handlers);
  }

  put(path: string, ...handlers: Handler[]): this;
  put(handler: Handler, ...handlers: Handler[]): this;
  put(pathOrHandler: string | Handler, ...handlers: Handler[]): this {
    return this.#register("PUT", pathOrHandler, handlers);
  }

  delete(path: string, ...handlers: Handler[]): this;
  delete(handler: Handler, ...handlers: Handler[]): this;
  delete(pathOrHandler: string | Handler, ...handlers: Handler[]): this {
    return this.#register("DELETE", pathOrHandler, handlers);
  }

  query(path: string, ...handlers: Handler[]): this;
  query(handler: Handler, ...handlers: Handler[]): this;
  query(pathOrHandler: string | Handler, ...handlers: Handler[]): this {
    return this.#register("QUERY", pathOrHandler, handlers);
  }

  all(path: string, ...handlers: Handler[]): this;
  all(handler: Handler, ...handlers: Handler[]): this;
  all(pathOrHandler: string | Handler, ...handlers: Handler[]): this {
    return this.#register("ALL", pathOrHandler, handlers);
  }

  on(method: string | string[], path: string | string[], ...handlers: Handler[]): this {
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

  use(handler: Handler, ...handlers: Handler[]): this;
  use(path: string, handler: Handler, ...handlers: Handler[]): this;
  use(pathOrHandler: string | Handler, ...handlers: Handler[]): this {
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
  route(path: string, app?: Server): this {
    if (!app) return this;
    const sub = this.basePath(path);
    const onError = app.#core.errorCustom ? app.#core.onError : undefined;
    for (const record of app.#core.routes) {
      if (!belongsTo(record.basePath, app.#basePath)) continue;
      const handler = record.handler;
      sub.#addRoute(
        record.method,
        record.path,
        onError
          ? async (c, next) => {
              try {
                return await handler(c, next);
              } catch (error) {
                if (error instanceof Error) return onError(error, c);
                throw error;
              }
            }
          : handler,
      );
    }
    return this;
  }

  /** Return a view of this server that prefixes later routes with `path`. */
  basePath(path: string): Server {
    return new Server(undefined, { core: this.#core, basePath: mergePath(this.#basePath, path) });
  }

  notFound(handler: NotFoundHandler): this {
    this.#core.notFound = handler;
    this.#core.notFoundCustom = true;
    return this;
  }

  onError(handler: ErrorHandler): this {
    this.#core.onError = handler;
    this.#core.errorCustom = true;
    return this;
  }

  /**
   * Forward matching requests to another server or fetch function.
   * The mounted handler sees the request path with `path` removed.
   */
  mount(path: string, application: Server | MountHandler, options?: MountOptions): this {
    const prefix = mergePath(this.#basePath, path).replace(/\/$/, "") || "/";
    const handler: Handler = async (c) => {
      const request = mountedRequest(c.req.raw, prefix, options);
      if (typeof application === "function") return application(request, c.env, c.executionCtx);
      return application.fetch(request, c.env, c.executionCtx);
    };
    this.#addRoute("ALL", mergePath(path, "*"), handler);
    return this;
  }

  /**
   * @deprecated Service Worker hosts should register fetch themselves.
   * In this runtime this registers a global `fetch` listener when `addEventListener` exists.
   */
  fire(): void {
    const target = globalThis as typeof globalThis & {
      addEventListener?: (
        type: string,
        listener: (event: {
          request: globalThis.Request;
          respondWith: (response: Response | Promise<Response>) => void;
        }) => void,
      ) => void;
    };
    if (typeof target.addEventListener !== "function") {
      throw new Error("server.fire() requires a global addEventListener('fetch') host");
    }
    target.addEventListener("fetch", (event) => {
      event.respondWith(this.fetch(event.request));
    });
  }

  /** Dispatch a Fetch request. This is the programmatic entry point. */
  fetch(request: globalThis.Request, env?: unknown, executionCtx?: unknown): Promise<Response> {
    if (request.method === "HEAD") {
      return this.#dispatch(request, env, executionCtx, "GET").then(
        (response) => new Response(null, response),
      );
    }
    return this.#dispatch(request, env, executionCtx, request.method.toUpperCase());
  }

  /** Test helper. A string is a path or absolute URL; a GET is the default. */
  request(
    input: globalThis.Request | string | URL,
    requestInit?: RequestInit,
    env?: unknown,
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
    if (this.#keepAlive) throw new Error("server already started");
    let started = false;
    // Tokio's listener and pending start future are not libuv handles.
    this.#keepAlive = setInterval(() => {
      if (started && !this.listening) clearInterval(this.#keepAlive);
    }, 1000);
    try {
      const port = await this.#core.native.start();
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
      await this.#core.native.stop();
    } finally {
      clearInterval(this.#keepAlive);
      this.#keepAlive = undefined;
    }
  }

  #register(method: string, pathOrHandler: string | Handler, handlers: Handler[]): this {
    const queue = [...handlers];
    if (typeof pathOrHandler === "string") this.#path = pathOrHandler;
    else queue.unshift(pathOrHandler);
    for (const handler of queue) this.#addRoute(method, this.#path, handler);
    return this;
  }

  #addRoute(method: string, path: string, handler: Handler): void {
    const full = mergePath(this.#basePath, path);
    this.#core.routes.push({ method, path: full, basePath: this.#basePath, handler });
    this.#core.router.add(method, full, handler);
  }

  async #dispatch(
    request: globalThis.Request,
    env: unknown,
    executionCtx: unknown,
    method: string,
  ): Promise<Response> {
    const path = this.#core.getPath(request, { env });
    const matched = this.#core.router.match(method, path);
    if (matched.length > 0) {
      const context = new Context(request, env, executionCtx, matched[0]?.params ?? {});
      return compose(matched, context, this.#core.notFound, this.#core.onError);
    }
    const legacy = await this.#legacy(request);
    if (legacy) return legacy;
    if (this.#core.routes.length > 0 || this.#core.notFoundCustom) {
      const context = new Context(request, env, executionCtx, {});
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

  async #handleWire(input: string): Promise<string> {
    try {
      const wire = JSON.parse(input) as {
        port: number;
        method: string;
        path: string;
        headers: [string, string][];
        body: string;
      };
      const host = this.host.includes(":") ? `[${this.host}]` : this.host;
      const request = new Request(`http://${host}:${wire.port}${wire.path}`, {
        method: wire.method,
        headers: wire.headers,
        body: ["GET", "HEAD"].includes(wire.method) ? undefined : Buffer.from(wire.body, "base64"),
      });
      const response = await this.fetch(request);
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
  }
}

async function compose(
  routes: { handler: Handler; params: Record<string, string> }[],
  context: Context,
  notFound: NotFoundHandler,
  onError: ErrorHandler,
): Promise<Response> {
  let index = -1;
  const run = async (position: number): Promise<void> => {
    if (position <= index) throw new Error("next() called multiple times");
    index = position;
    const route = routes[position];
    let result: unknown;
    try {
      if (route) {
        context.req.assignParams(route.params);
        result = await route.handler(context, () => run(position + 1));
      } else result = await notFound(context);
    } catch (error) {
      if (error instanceof Error) result = await onError(error, context);
      else throw error;
    }
    if (result instanceof globalThis.Response && !context.finalized) context.finalize(result);
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
