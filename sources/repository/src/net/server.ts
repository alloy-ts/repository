import { RocketServer } from "../../../../dist/index.js";
import { File } from "../fs/file.ts";
import { Request, Response } from "./request.ts";
import { SmartRouter, type Router } from "./router/reg-exp-router.ts";

export { SmartRouter, RegExpRouter, type Router } from "./router/reg-exp-router.ts";

export type Next = () => Promise<void>;

type ResponseHeadersInit =
  | Headers
  | string[][]
  | Record<string, string | readonly string[]>
  | Iterable<[string, string]>;
type ResponseBodyInit =
  | ArrayBuffer
  | ArrayBufferView
  | Blob
  | FormData
  | URLSearchParams
  | ReadableStream<Uint8Array>
  | string;

function tryDecode(str: string): string {
  try {
    return decodeURIComponent(str);
  } catch {
    return str;
  }
}

export class HonoRequest {
  readonly raw: Request;
  #params: Record<string, string>;
  readonly #url: URL;

  constructor(raw: Request, params: Record<string, string> = {}) {
    this.raw = raw;
    this.#params = params;
    this.#url = new URL(raw.url);
  }

  _setParams(params: Record<string, string>): void {
    this.#params = { ...this.#params, ...params };
  }

  get url(): string {
    return this.raw.url;
  }

  get method(): string {
    return this.raw.method;
  }

  get headers(): Headers {
    return this.raw.headers;
  }

  get body(): ReadableStream<Uint8Array> | null {
    return this.raw.body;
  }

  get bodyUsed(): boolean {
    return this.raw.bodyUsed;
  }

  get signal(): AbortSignal {
    return this.raw.signal;
  }

  param(): Record<string, string>;
  param(key: string): string | undefined;
  param(key?: string): Record<string, string> | string | undefined {
    if (key === undefined) {
      return { ...this.#params };
    }
    return this.#params[key];
  }

  query(): Record<string, string>;
  query(key: string): string | undefined;
  query(key?: string): Record<string, string> | string | undefined {
    if (key === undefined) {
      const result: Record<string, string> = {};
      this.#url.searchParams.forEach((val, k) => {
        result[k] = val;
      });
      return result;
    }
    return this.#url.searchParams.get(key) ?? undefined;
  }

  header(name?: string): string | undefined | Record<string, string> {
    if (name === undefined) {
      const result: Record<string, string> = {};
      this.raw.headers.forEach((val, k) => {
        result[k] = val;
      });
      return result;
    }
    return this.raw.headers.get(name) ?? undefined;
  }

  async json<T = any>(): Promise<T> {
    return (await this.raw.json()) as T;
  }

  async text(): Promise<string> {
    return await this.raw.text();
  }

  async arrayBuffer(): Promise<ArrayBuffer> {
    return await this.raw.arrayBuffer();
  }

  async blob(): Promise<Blob> {
    return await this.raw.blob();
  }

  async formData(): Promise<FormData> {
    return await this.raw.formData();
  }
}

type EnvConstraint = {
  Bindings?: Record<string, unknown>;
  Variables?: Record<string, unknown>;
};

export class Context<
  E extends EnvConstraint = {
    Bindings: Record<string, unknown>;
    Variables: Record<string, unknown>;
  },
> {
  readonly req: HonoRequest;
  readonly env: E["Bindings"];
  readonly executionCtx?: any;
  readonly #varMap = new Map<string, unknown>();
  readonly #notFoundHandler?: NotFoundHandler<E>;
  #status: number = 200;
  #headers: Headers;
  #res?: Response;

  constructor(
    req: Request,
    options: {
      params?: Record<string, string>;
      env?: any;
      executionCtx?: any;
      notFoundHandler?: NotFoundHandler<E>;
    } = {},
  ) {
    this.req = new HonoRequest(req, options.params);
    this.env = options.env ?? ({} as any);
    this.executionCtx = options.executionCtx;
    this.#notFoundHandler = options.notFoundHandler;
    this.#headers = new Headers();
  }

  set<Key extends keyof E["Variables"] & string>(key: Key, value: E["Variables"][Key]): void;
  set<Key extends string>(key: Key, value: unknown): void;
  set(key: string, value: unknown): void {
    this.#varMap.set(key, value);
  }

  get<Key extends keyof E["Variables"] & string>(key: Key): E["Variables"][Key];
  get<Key extends string>(key: Key): any;
  get(key: string): any {
    return this.#varMap.get(key);
  }

  header(name: string, value: string, options?: { append?: boolean }): void {
    if (options?.append) {
      this.#headers.append(name, value);
    } else {
      this.#headers.set(name, value);
    }
    if (this.#res) {
      if (options?.append) {
        this.#res.headers.append(name, value);
      } else {
        this.#res.headers.set(name, value);
      }
    }
  }

  status(status: number): void {
    this.#status = status;
    if (this.#res) {
      this.#res = new Response(this.#res.body, {
        status,
        statusText: this.#res.statusText,
        headers: this.#res.headers,
      });
    }
  }

  get res(): Response | undefined {
    return this.#res;
  }

  set res(response: Response | undefined) {
    this.#res = response;
  }

  #mergeHeaders(initHeaders?: ResponseHeadersInit): Headers {
    const combined = new Headers(this.#headers);
    if (initHeaders) {
      const extra = new Headers(initHeaders as any);
      extra.forEach((v, k) => {
        combined.set(k, v);
      });
    }
    return combined;
  }

  text(text: string, status?: number, headers?: ResponseHeadersInit): Response {
    const s = status ?? this.#status;
    const h = this.#mergeHeaders(headers);
    let res: Response;
    if (!h.has("content-type")) {
      h.set("content-type", "text/plain; charset=UTF-8");
      res = new Response(text, { status: s, headers: h });
    } else {
      res = new Response(text, { status: s, headers: h });
    }
    this.#res = res;
    return res;
  }

  json<T = any>(object: T, status?: number, headers?: ResponseHeadersInit): Response {
    const s = status ?? this.#status;
    const h = this.#mergeHeaders(headers);
    if (!h.has("content-type")) {
      h.set("content-type", "application/json; charset=UTF-8");
    }
    const body = JSON.stringify(object);
    const res = new Response(body, { status: s, headers: h });
    this.#res = res;
    return res;
  }

  html(html: string, status?: number, headers?: ResponseHeadersInit): Response {
    const s = status ?? this.#status;
    const h = this.#mergeHeaders(headers);
    if (!h.has("content-type")) {
      h.set("content-type", "text/html; charset=UTF-8");
    }
    const res = new Response(html, { status: s, headers: h });
    this.#res = res;
    return res;
  }

  body(data: ResponseBodyInit | null, status?: number, headers?: ResponseHeadersInit): Response {
    const s = status ?? this.#status;
    const h = this.#mergeHeaders(headers);
    const res = new Response(data as any, { status: s, headers: h });
    this.#res = res;
    return res;
  }

  redirect(url: string, status: number = 302): Response {
    const res = Response.redirect(url, status);
    this.#res = res;
    return res;
  }

  notFound(): Response | Promise<Response> {
    if (this.#notFoundHandler) {
      const res = this.#notFoundHandler(this);
      if (res instanceof globalThis.Response) {
        this.#res = res instanceof Response ? res : new Response(res.body, res);
        return this.#res;
      }
      return Promise.resolve(res).then((r) => {
        this.#res = r instanceof Response ? r : new Response(r.body, r);
        return this.#res;
      });
    }
    const res = new Response("404 Not Found", {
      status: 404,
      headers: this.#headers,
    });
    this.#res = res;
    return res;
  }
}

export type Handler<E extends EnvConstraint = any> = (
  c: Context<E>,
  next: Next,
) => Response | Promise<Response | void> | void;
export type MiddlewareHandler<E extends EnvConstraint = any> = Handler<E>;
export type NotFoundHandler<E extends EnvConstraint = any> = (
  c: Context<E>,
) => Response | Promise<Response>;
export type ErrorHandler<E extends EnvConstraint = any> = (
  err: any,
  c: Context<E>,
) => Response | Promise<Response>;

type LegacyRoutedRequest = Request & { params: Record<string, string> };
type LegacyHandler = (request: LegacyRoutedRequest) => Response | Promise<Response>;
type LegacyRoute =
  | Response
  | File
  | LegacyHandler
  | Partial<Record<string, LegacyHandler | Response | File>>;

export interface ServerOptions<_E extends EnvConstraint = any> {
  port?: number;
  host?: string;
  strict?: boolean;
  router?: Router<any>;
  getPath?: (req: Request | globalThis.Request) => string;
  routes?: Record<string, LegacyRoute>;
  fetch?: LegacyHandler;
}

interface RouteEntry {
  method: string; // 'GET', 'POST', ..., 'ALL'
  path: string; // pattern
  isMiddleware: boolean;
  matcher: (pathname: string) => Record<string, string> | undefined;
  handlers: Handler[];
}

export class Server<
  E extends EnvConstraint = {
    Bindings: Record<string, unknown>;
    Variables: Record<string, unknown>;
  },
> {
  #native?: RocketServer;
  readonly host: string;
  readonly portOption?: number;
  readonly strict: boolean;
  readonly router: Router<Handler<E>[]>;
  readonly getPath?: (req: Request | globalThis.Request) => string;
  #keepAlive?: ReturnType<typeof setInterval>;

  readonly #routes: RouteEntry[] = [];
  #hasCustomRouter = false;
  #lastRegisteredPath?: string;
  #notFoundHandler?: NotFoundHandler<E>;
  #errorHandler?: ErrorHandler<E>;
  #basePath = "";
  #parentServer?: Server<any>;
  #legacyRoutes?: Record<string, LegacyRoute>;
  #legacyFetch?: LegacyHandler;

  constructor(options: ServerOptions<E> = {}) {
    this.host = options.host ?? "127.0.0.1";
    this.portOption = options.port;
    this.strict = options.strict ?? true;
    if (options.router) {
      this.router = options.router as unknown as Router<Handler<E>[]>;
      this.#hasCustomRouter = true;
    } else {
      this.router = new SmartRouter<Handler<E>[]>();
    }
    this.getPath = options.getPath;
    this.#legacyRoutes = options.routes;
    this.#legacyFetch = options.fetch;
  }

  get port(): number {
    return this.#native ? this.#native.port : (this.portOption ?? 3000);
  }

  get listening(): boolean {
    return this.#native ? this.#native.listening : false;
  }

  basePath(path: string): Server<E> {
    const cleaned = ("/" + path.replace(/^\/+|\/+$/g, "")).replace(/\/+$/, "");
    const sub = new Server<E>({
      strict: this.strict,
      router: this.router,
      getPath: this.getPath,
    });
    sub.#basePath = this.#basePath + (cleaned === "/" ? "" : cleaned);
    sub.#parentServer = this;
    return sub;
  }

  notFound(handler: NotFoundHandler<E>): this {
    this.#notFoundHandler = handler;
    return this;
  }

  onError(handler: ErrorHandler<E>): this {
    this.#errorHandler = handler;
    return this;
  }

  use(arg1: string | MiddlewareHandler<E>, ...handlers: MiddlewareHandler<E>[]): this {
    if (typeof arg1 === "string") {
      this.#addRoute("ALL", arg1, handlers, true);
    } else {
      this.#addRoute("ALL", "*", [arg1, ...handlers], true);
    }
    return this;
  }

  on(methods: string | string[], paths: string | string[], ...handlers: Handler<E>[]): this {
    const methodList = Array.isArray(methods) ? methods : [methods];
    const pathList = Array.isArray(paths) ? paths : [paths];
    for (const m of methodList) {
      for (const p of pathList) {
        this.#addRoute(m.toUpperCase(), p, handlers, false);
      }
    }
    return this;
  }

  all(pathOrHandler: string | Handler<E>, ...handlers: Handler<E>[]): this {
    return this.#registerMethodRoute("ALL", pathOrHandler, handlers);
  }

  get(pathOrHandler: string | Handler<E>, ...handlers: Handler<E>[]): this {
    return this.#registerMethodRoute("GET", pathOrHandler, handlers);
  }

  post(pathOrHandler: string | Handler<E>, ...handlers: Handler<E>[]): this {
    return this.#registerMethodRoute("POST", pathOrHandler, handlers);
  }

  put(pathOrHandler: string | Handler<E>, ...handlers: Handler<E>[]): this {
    return this.#registerMethodRoute("PUT", pathOrHandler, handlers);
  }

  delete(pathOrHandler: string | Handler<E>, ...handlers: Handler<E>[]): this {
    return this.#registerMethodRoute("DELETE", pathOrHandler, handlers);
  }

  patch(pathOrHandler: string | Handler<E>, ...handlers: Handler<E>[]): this {
    return this.#registerMethodRoute("PATCH", pathOrHandler, handlers);
  }

  options(pathOrHandler: string | Handler<E>, ...handlers: Handler<E>[]): this {
    return this.#registerMethodRoute("OPTIONS", pathOrHandler, handlers);
  }

  head(pathOrHandler: string | Handler<E>, ...handlers: Handler<E>[]): this {
    return this.#registerMethodRoute("HEAD", pathOrHandler, handlers);
  }

  query(pathOrHandler: string | Handler<E>, ...handlers: Handler<E>[]): this {
    return this.#registerMethodRoute("QUERY", pathOrHandler, handlers);
  }

  route(path: string, subServer?: Server<any>): this {
    if (!subServer) return this;
    const prefix = ("/" + path.replace(/^\/+|\/+$/g, "")).replace(/\/+$/, "");
    for (const r of subServer.#routes) {
      const combinedPath =
        r.path === "*"
          ? prefix === ""
            ? "/*"
            : prefix + "/*"
          : prefix === ""
            ? r.path
            : prefix + (r.path === "/" ? "" : r.path);
      this.#routes.push({
        method: r.method,
        path: combinedPath,
        isMiddleware: r.isMiddleware,
        matcher: buildMatcher(combinedPath, this.strict, r.isMiddleware),
        handlers: r.handlers.map((h) => {
          return async (c, next) => {
            try {
              return await h(c, next);
            } catch (err) {
              if (subServer.#errorHandler) {
                const errRes = await subServer.#errorHandler(err, c);
                c.res = errRes instanceof Response ? errRes : new Response(errRes.body, errRes);
                return c.res;
              }
              throw err;
            }
          };
        }),
      });
    }
    return this;
  }

  mount(
    path: string,
    app:
      | Server<any>
      | ((request: globalThis.Request, ...args: any[]) => any)
      | { fetch: (request: globalThis.Request, ...args: any[]) => any },
    _options?: any,
  ): this {
    const prefix = ("/" + path.replace(/^\/+|\/+$/g, "")).replace(/\/+$/, "");
    const mountPath = prefix === "" ? "/*" : `${prefix}/*`;
    const handler: Handler<E> = async (c) => {
      const req = c.req.raw;
      const strippedUrl = new URL(req.url);
      let newPathname = strippedUrl.pathname.slice(prefix.length);
      if (!newPathname.startsWith("/")) newPathname = "/" + newPathname;
      strippedUrl.pathname = newPathname;
      const mountedReq = new Request(strippedUrl.toString(), req);

      if (app instanceof Server) {
        return await app.fetch(mountedReq, c.env, c.executionCtx);
      }
      if (typeof (app as any).fetch === "function") {
        return await (app as any).fetch(mountedReq, c.env, c.executionCtx);
      }
      if (typeof app === "function") {
        return await (app as any)(mountedReq, c.env, c.executionCtx);
      }
    };
    this.#addRoute("ALL", mountPath, [handler], false);
    if (prefix !== "") {
      this.#addRoute("ALL", prefix, [handler], false);
    }
    return this;
  }

  fire(): void {
    const g = globalThis as any;
    if (typeof g.addEventListener === "function") {
      g.addEventListener("fetch", (event: any): void => {
        event.respondWith(this.fetch(event.request, g.env, event as any));
      });
    }
  }

  async request(
    input: string | URL | globalThis.Request,
    options?: RequestInit,
    env?: any,
    executionCtx?: any,
  ): Promise<Response> {
    let req: Request;
    if (typeof input === "string" || input instanceof URL) {
      const urlStr = typeof input === "string" ? input : input.toString();
      const validUrl =
        urlStr.startsWith("http://") || urlStr.startsWith("https://")
          ? urlStr
          : `http://localhost${urlStr.startsWith("/") ? "" : "/"}${urlStr}`;
      req = new Request(validUrl, options);
    } else {
      req = new Request(input, options);
    }
    return await this.fetch(req, env, executionCtx);
  }

  fetch = async (request: globalThis.Request, env?: any, executionCtx?: any): Promise<Response> => {
    const req = request instanceof Request ? request : new Request(request);
    const method = req.method.toUpperCase();

    const c = new Context<E>(req, {
      env,
      executionCtx,
      notFoundHandler: this.#notFoundHandler,
    });

    try {
      let pathname: string;
      if (this.getPath) {
        pathname = this.getPath(req);
      } else {
        pathname = new URL(req.url).pathname;
      }

      // First check legacy options.routes if present
      if (this.#legacyRoutes) {
        const legacyResult = await this.#dispatchLegacy(req);
        if (legacyResult) {
          if (method === "HEAD") {
            return new Response(null, {
              status: legacyResult.status,
              statusText: legacyResult.statusText,
              headers: legacyResult.headers,
            });
          }
          return legacyResult;
        }
      }

      // Match routes
      const matched: { handler: Handler<E>; params: Record<string, string> }[] = [];

      if (this.#hasCustomRouter) {
        let routerMatches = this.router.match(method, pathname);
        if (method === "HEAD") {
          const explicitHeadMatches = routerMatches.filter(
            ([handlers]) => (handlers as any)._method === "HEAD",
          );
          if (explicitHeadMatches.length === 0) {
            const getMatches = this.router.match("GET", pathname);
            routerMatches = [...routerMatches, ...getMatches];
          }
        }
        for (const [handlers, params] of routerMatches) {
          for (const h of handlers) {
            matched.push({ handler: h, params });
          }
        }
      } else {
        let hasExplicitHeadMatch = false;
        if (method === "HEAD") {
          for (const r of this.#routes) {
            if (r.method === "HEAD" && !r.isMiddleware && r.matcher(pathname) !== undefined) {
              hasExplicitHeadMatch = true;
              break;
            }
          }
        }

        for (const r of this.#routes) {
          const isMatchMethod =
            r.method === "ALL" ||
            r.method === method ||
            (method === "HEAD" && r.method === "GET" && !hasExplicitHeadMatch);

          if (!isMatchMethod) {
            continue;
          }

          const params = r.matcher(pathname);
          if (params !== undefined) {
            for (const h of r.handlers) {
              matched.push({ handler: h, params });
            }
          }
        }
      }

      const dispatch = async (index: number): Promise<Response> => {
        if (index >= matched.length) {
          if (c.res) return c.res;
          if (this.#legacyFetch) {
            const routedReq = req as unknown as LegacyRoutedRequest;
            routedReq.params = {};
            const res = await this.#legacyFetch(routedReq);
            c.res = res instanceof Response ? res : new Response(res.body, res);
            return c.res;
          }
          if (this.#notFoundHandler) {
            const res = await this.#notFoundHandler(c);
            c.res = res instanceof Response ? res : new Response(res.body, res);
            return c.res;
          }
          const res = await c.notFound();
          c.res = res instanceof Response ? res : new Response(res.body, res);
          return c.res;
        }

        const { handler, params } = matched[index]!;
        c.req._setParams(params);

        let nextCalled = false;
        const next: Next = async () => {
          nextCalled = true;
          await dispatch(index + 1);
        };

        try {
          const result = await handler(c, next);
          if (result instanceof globalThis.Response) {
            c.res = result instanceof Response ? result : new Response(result.body, result);
            return c.res;
          }
        } catch (err) {
          if (this.#errorHandler) {
            const errRes = await this.#errorHandler(err, c);
            c.res = errRes instanceof Response ? errRes : new Response(errRes.body, errRes);
            return c.res;
          }
          throw err;
        }

        if (!nextCalled && !c.res) {
          const unfinalizedErr = new Error(
            "Context is not finalized. Did you forget to return a Response object or `await next()`?",
          );
          if (this.#errorHandler) {
            const errRes = await this.#errorHandler(unfinalizedErr, c);
            c.res = errRes instanceof Response ? errRes : new Response(errRes.body, errRes);
            return c.res;
          }
          throw unfinalizedErr;
        }

        return c.res ?? (await c.notFound());
      };

      const res = await dispatch(0);
      if (method === "HEAD") {
        const headers = new Headers(res.headers);
        if (!headers.has("content-length")) {
          const bodyBytes = (res as any).body
            ? Buffer.from(await res.clone().arrayBuffer()).length
            : undefined;
          if (bodyBytes !== undefined) {
            headers.set("content-length", String(bodyBytes));
          }
        }
        return new Response(null, {
          status: res.status,
          statusText: res.statusText,
          headers,
        });
      }
      return res;
    } catch (err) {
      let errRes: Response;
      if (this.#errorHandler) {
        errRes = await this.#errorHandler(err, c);
      } else {
        errRes = new Response("Internal Server Error", { status: 500 });
      }
      if (method === "HEAD") {
        const headers = new Headers(errRes.headers);
        if (!headers.has("content-length")) {
          const bodyBytes = (errRes as any).body
            ? Buffer.from(await errRes.clone().arrayBuffer()).length
            : undefined;
          if (bodyBytes !== undefined) {
            headers.set("content-length", String(bodyBytes));
          }
        }
        return new Response(null, {
          status: errRes.status,
          statusText: errRes.statusText,
          headers,
        });
      }
      return errRes;
    }
  };

  async #dispatchLegacy(req: Request): Promise<Response | undefined> {
    const sorted = Object.entries(this.#legacyRoutes ?? {}).sort(([a], [b]) => score(b) - score(a));
    const pathname = new URL(req.url).pathname;
    let selected: LegacyRoute | undefined;
    const routedReq = req as unknown as LegacyRoutedRequest;
    routedReq.params = {};

    for (const [pattern, route] of sorted) {
      const params = matchLegacy(pattern, pathname);
      if (params) {
        routedReq.params = params;
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
      selected = selected[req.method] ?? (req.method === "HEAD" ? selected["GET"] : undefined);
    }

    if (selected !== undefined) {
      if (selected instanceof File) {
        return new Response(await selected.bytes(), {
          headers: { "content-type": selected.type },
        });
      }
      if (typeof selected === "function") {
        return await selected(routedReq);
      }
      if (selected instanceof globalThis.Response) {
        return selected.clone();
      }
    }

    if (this.#routes.length === 0) {
      if (this.#legacyFetch) {
        return await this.#legacyFetch(routedReq);
      }
      return new Response("Not Found", { status: 404 });
    }

    return undefined;
  }

  async start(): Promise<number> {
    if (this.#keepAlive) throw new Error("server already started");
    let started = false;
    this.#keepAlive = setInterval(() => {
      if (started && !this.listening) clearInterval(this.#keepAlive);
    }, 1000);

    const host = this.host;
    this.#native = new RocketServer(this.portOption ?? 3000, this.host, async (input: string) => {
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
        );
        const response = await this.fetch(request);
        if (!(response instanceof globalThis.Response)) {
          throw new TypeError("route must return a Response");
        }
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
      if (this.#native) {
        await this.#native.stop();
      }
    } finally {
      clearInterval(this.#keepAlive);
      this.#keepAlive = undefined;
    }
  }

  #addRoute(method: string, path: string, handlers: Handler<E>[], isMiddleware: boolean): void {
    let normalized = path;
    if (this.#basePath) {
      const p =
        normalized === "*" ? "/*" : normalized.startsWith("/") ? normalized : "/" + normalized;
      normalized = this.#basePath + (p === "/" ? "" : p);
    } else {
      if (normalized !== "*" && !normalized.startsWith("/")) {
        normalized = "/" + normalized;
      }
    }

    this.#lastRegisteredPath = normalized;
    const matcher = buildMatcher(normalized, this.strict, isMiddleware);
    this.#routes.push({
      method,
      path: normalized,
      isMiddleware,
      matcher,
      handlers,
    });
    const taggedHandlers = [...handlers] as any;
    taggedHandlers._method = method;
    this.router.add(method, normalized, taggedHandlers);

    if (this.#parentServer) {
      this.#parentServer.#routes.push({
        method,
        path: normalized,
        isMiddleware,
        matcher: buildMatcher(normalized, this.#parentServer.strict, isMiddleware),
        handlers,
      });
      this.#parentServer.router.add(method, normalized, taggedHandlers);
    }
  }

  #registerMethodRoute(
    method: string,
    pathOrHandler: string | Handler<E>,
    handlers: Handler<E>[],
  ): this {
    if (typeof pathOrHandler === "string") {
      this.#addRoute(method, pathOrHandler, handlers, false);
    } else {
      const path = this.#lastRegisteredPath ?? "*";
      this.#addRoute(method, path, [pathOrHandler, ...handlers], false);
    }
    return this;
  }
}

function score(pattern: string): number {
  return pattern
    .split("/")
    .reduce((n, part) => n + (part === "*" ? -1000 : part.startsWith(":") ? 1 : 10), 0);
}

function matchLegacy(pattern: string, path: string): Record<string, string> | undefined {
  const parts = pattern.split("/");
  const actual = path.split("/");
  const params: Record<string, string> = {};
  for (let i = 0; i < parts.length; i++) {
    const part = parts[i]!;
    const segment = actual[i];
    if (part === "*") return params;
    if (segment === undefined) return;
    if (part.startsWith(":")) params[part.slice(1)] = tryDecode(segment);
    else if (part !== segment) return;
  }
  return parts.length === actual.length ? params : undefined;
}

function extractBracedParams(pattern: string): {
  transformed: string;
  custom: { ph: string; name: string; regex: string }[];
} {
  let result = "";
  let i = 0;
  const custom: { ph: string; name: string; regex: string }[] = [];
  while (i < pattern.length) {
    if (pattern[i] === ":" && /[a-zA-Z0-9_]/.test(pattern[i + 1] || "")) {
      const nameStart = i + 1;
      let nameEnd = nameStart;
      while (nameEnd < pattern.length && /[a-zA-Z0-9_]/.test(pattern[nameEnd]!)) {
        nameEnd++;
      }
      const name = pattern.slice(nameStart, nameEnd);
      if (pattern[nameEnd] === "{") {
        let depth = 1;
        const regexStart = nameEnd + 1;
        let j = regexStart;
        while (j < pattern.length && depth > 0) {
          if (pattern[j] === "{") depth++;
          else if (pattern[j] === "}") depth--;
          j++;
        }
        if (depth === 0) {
          const regex = pattern.slice(regexStart, j - 1);
          const ph = `__PARAM_CUSTOM_${custom.length}__`;
          custom.push({ ph, name, regex });
          result += ph;
          i = j;
          continue;
        }
      }
    }
    result += pattern[i];
    i++;
  }
  return { transformed: result, custom };
}

function buildMatcher(
  pattern: string,
  strict: boolean,
  _isMiddleware: boolean,
): (pathname: string) => Record<string, string> | undefined {
  if (pattern === "*") {
    return () => ({});
  }

  const { transformed, custom } = extractBracedParams(pattern);

  const optionalParams: { ph: string; name: string }[] = [];
  let transformedPattern = transformed.replace(/\/:([a-zA-Z0-9_]+)\?/g, (_match, name) => {
    const ph = `/__PARAM_OPTIONAL_${optionalParams.length}__`;
    optionalParams.push({ ph, name });
    return ph;
  });

  const isTrailingWildcard = transformedPattern.endsWith("/*");
  if (isTrailingWildcard) {
    transformedPattern = transformedPattern.slice(0, -2);
  }

  const parts = transformedPattern.split("/").filter(Boolean);
  let regexStr = "^";
  const paramNames: string[] = [];

  if (parts.length === 0) {
    regexStr += "\\/";
  } else {
    for (let i = 0; i < parts.length; i++) {
      const part = parts[i]!;
      const opt = optionalParams.find((o) => o.ph === `/${part}`);
      if (opt) {
        paramNames.push(opt.name);
        regexStr += `(?:\\/(?<p_${opt.name}>[^/]+))?`;
        continue;
      }

      regexStr += "\\/";

      const cust = custom.find((c) => c.ph === part);
      if (cust) {
        paramNames.push(cust.name);
        regexStr += `(?<p_${cust.name}>${cust.regex})`;
      } else if (part === "*") {
        if (i === parts.length - 1) {
          regexStr += "(?:.*)";
        } else {
          regexStr += "(?:[^/]+)";
        }
      } else if (part.startsWith(":")) {
        const name = part.slice(1);
        paramNames.push(name);
        regexStr += `(?<p_${name}>[^/]+)`;
      } else {
        regexStr += escapeRegex(part);
      }
    }
  }

  if (isTrailingWildcard) {
    if (parts.length === 0) {
      regexStr += "(?:.*)?$";
    } else {
      regexStr += "(?:|/.*)$";
    }
  } else if (!strict) {
    if (parts.length === 0) {
      regexStr += "?$";
    } else {
      regexStr += "\\/?$";
    }
  } else {
    regexStr += "$";
  }

  const regex = new RegExp(regexStr);

  return (pathname: string) => {
    const m = regex.exec(pathname);
    if (!m) return undefined;
    const params: Record<string, string> = {};
    if (m.groups) {
      for (const name of paramNames) {
        const val = m.groups[`p_${name}`];
        if (val !== undefined) {
          params[name] = tryDecode(val);
        }
      }
    }
    return params;
  };
}

function escapeRegex(str: string): string {
  return str.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
