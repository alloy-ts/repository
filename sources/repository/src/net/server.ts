import { RocketServer } from "../../../../dist/index.js";
import { File } from "../fs/file.ts";
import { Request, Response } from "./request.ts";

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

export class Context<
  E extends { Bindings?: Record<string, unknown>; Variables?: Record<string, unknown> } = {
    Bindings: Record<string, unknown>;
    Variables: Record<string, unknown>;
  },
> {
  readonly req: HonoRequest;
  readonly env: E["Bindings"];
  readonly executionCtx?: any;
  readonly #varMap = new Map<string, unknown>();
  #status: number = 200;
  #headers: Headers;
  #res?: Response;

  constructor(
    req: Request,
    options: {
      params?: Record<string, string>;
      env?: any;
      executionCtx?: any;
    } = {},
  ) {
    this.req = new HonoRequest(req, options.params);
    this.env = options.env ?? ({} as any);
    this.executionCtx = options.executionCtx;
    this.#headers = new Headers();
  }

  set<Key extends string>(key: Key, value: unknown): void {
    this.#varMap.set(key, value);
  }

  get<Key extends string>(key: Key): any {
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
      // Recreate response with updated status
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

  notFound(): Response {
    const res = new Response("404 Not Found", {
      status: 404,
      headers: this.#headers,
    });
    this.#res = res;
    return res;
  }
}

type EnvConstraint = {
  Bindings?: Record<string, unknown>;
  Variables?: Record<string, unknown>;
};

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
  router?: any;
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
  E extends { Bindings?: Record<string, unknown>; Variables?: Record<string, unknown> } = {
    Bindings: Record<string, unknown>;
    Variables: Record<string, unknown>;
  },
> {
  #native?: RocketServer;
  readonly host: string;
  readonly portOption?: number;
  readonly strict: boolean;
  readonly getPath?: (req: Request | globalThis.Request) => string;
  #keepAlive?: ReturnType<typeof setInterval>;

  readonly #routes: RouteEntry[] = [];
  #lastRegisteredPath: string = "/";
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
      const combinedPath = r.path === "*" ? prefix + "/*" : prefix + (r.path === "/" ? "" : r.path);
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
                return await subServer.#errorHandler(err, c);
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
    return await this.fetch(req);
  }

  fetch = async (request: globalThis.Request, env?: any, executionCtx?: any): Promise<Response> => {
    const req = request instanceof Request ? request : new Request(request);
    let pathname: string;
    if (this.getPath) {
      pathname = this.getPath(req);
    } else {
      pathname = new URL(req.url).pathname;
    }

    // First check legacy options.routes if present
    if (this.#legacyRoutes) {
      const legacyResult = await this.#dispatchLegacy(req);
      if (legacyResult) return legacyResult;
    }

    // Match routes
    const matched: { handler: Handler<E>; params: Record<string, string> }[] = [];
    const method = req.method.toUpperCase();

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

    const c = new Context<E>(req, { env, executionCtx });

    const dispatch = async (index: number): Promise<Response> => {
      if (index >= matched.length) {
        if (c.res) return c.res;
        if (this.#legacyFetch) {
          const routedReq = req as unknown as LegacyRoutedRequest;
          routedReq.params = {};
          return await this.#legacyFetch(routedReq);
        }
        if (this.#notFoundHandler) {
          return await this.#notFoundHandler(c);
        }
        return c.notFound();
      }

      const { handler, params } = matched[index]!;
      // Update req params
      c.req._setParams(params);

      let nextCalled = false;
      const next: Next = async () => {
        nextCalled = true;
        await dispatch(index + 1);
      };

      const result = await handler(c, next);
      if (result instanceof globalThis.Response) {
        c.res = result instanceof Response ? result : new Response(result.body, result);
        return c.res;
      }
      if (!nextCalled && index + 1 < matched.length) {
        // Automatically proceed if return wasn't a Response and next wasn't explicitly called
        await dispatch(index + 1);
      }
      return c.res ?? c.notFound();
    };

    try {
      const res = await dispatch(0);
      if (method === "HEAD") {
        return new Response(null, {
          status: res.status,
          statusText: res.statusText,
          headers: res.headers,
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
        return new Response(null, {
          status: errRes.status,
          statusText: errRes.statusText,
          headers: errRes.headers,
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
    if (normalized !== "*" && !normalized.startsWith("/")) {
      normalized = "/" + normalized;
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
    if (this.#parentServer) {
      const combinedPath =
        normalized === "*"
          ? this.#basePath + "/*"
          : this.#basePath + (normalized === "/" ? "" : normalized);
      this.#parentServer.#addRoute(method, combinedPath, handlers, isMiddleware);
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
      // Chained route: reuse the last registered route path if available, or fallback to "*"
      const path =
        handlers.length === 0 && typeof pathOrHandler === "function" && this.#routes.length > 0
          ? this.#lastRegisteredPath
          : "*";
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
    if (part.startsWith(":")) params[part.slice(1)] = decodeURIComponent(segment);
    else if (part !== segment) return;
  }
  return parts.length === actual.length ? params : undefined;
}

function buildMatcher(
  pattern: string,
  strict: boolean,
  isMiddleware: boolean,
): (pathname: string) => Record<string, string> | undefined {
  if (pattern === "*") {
    return () => ({});
  }

  // Parse pattern into regex
  // Patterns can have:
  // - :name{[regex]} (e.g. :date{[0-9]+}, :filename{.+\\.png})
  // - :name? (optional param)
  // - :name (standard param)
  // - * (wildcard)
  // - static segments

  const paramNames: string[] = [];
  let regexStr = "^";

  // Replace :name{pattern} first by placeholders
  const customRegExps: { placeholder: string; name: string; pattern: string }[] = [];
  let transformedPattern = pattern.replace(
    /:([a-zA-Z0-9_]+)\{([^}]+)\}/g,
    (_match, name, regex) => {
      const placeholder = `__PARAM_CUSTOM_${customRegExps.length}__`;
      customRegExps.push({ placeholder, name, pattern: regex });
      return placeholder;
    },
  );

  // Handle optional parameters: /path/:param?
  const optionalParams: { placeholder: string; name: string }[] = [];
  transformedPattern = transformedPattern.replace(/\/:([a-zA-Z0-9_]+)\?/g, (_match, name) => {
    const placeholder = `/__PARAM_OPTIONAL_${optionalParams.length}__`;
    optionalParams.push({ placeholder, name });
    return placeholder;
  });

  const parts = transformedPattern.split("/").filter(Boolean);

  if (parts.length === 0) {
    // Root path "/"
    regexStr += "\\/";
  } else {
    for (let i = 0; i < parts.length; i++) {
      const part = parts[i]!;
      const opt = optionalParams.find((o) => o.placeholder === `/${part}`);
      if (opt) {
        paramNames.push(opt.name);
        regexStr += `(?:\\/([^/]+))?`;
        continue;
      }

      regexStr += "\\/";

      const custom = customRegExps.find((c) => c.placeholder === part);
      if (custom) {
        paramNames.push(custom.name);
        regexStr += `(${custom.pattern})`;
      } else if (part === "*") {
        if (i === parts.length - 1) {
          regexStr += "(?:.*)";
        } else {
          regexStr += "(?:[^/]+)";
        }
      } else if (part.startsWith(":")) {
        const name = part.slice(1);
        paramNames.push(name);
        regexStr += "([^/]+)";
      } else {
        regexStr += escapeRegex(part);
      }
    }
  }

  if (isMiddleware) {
    regexStr += "(?:\\/.*)?$";
  } else if (!strict) {
    if (pattern === "/") {
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
    for (let i = 0; i < paramNames.length; i++) {
      const val = m[i + 1];
      if (val !== undefined) {
        params[paramNames[i]!] = decodeURIComponent(val);
      }
    }
    return params;
  };
}

function escapeRegex(str: string): string {
  return str.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
