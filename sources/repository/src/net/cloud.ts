export { createApp } from "./app.ts";
export type { App, AppContext, AppOptions, Handler, Route, RoutedRequest } from "./app.ts";
import type { App } from "./app.ts";

export const targetCapabilities = {
  rocket: { streaming: true, native: true },
  cloudflare: { streaming: true, native: false },
  lambda: { streaming: false, native: true },
  google: { streaming: false, native: true },
  azure: { streaming: false, native: true },
} as const;
export type Target = keyof typeof targetCapabilities;
export function requireCapabilities(
  target: Target,
  needs: { streaming?: boolean; native?: boolean },
): void {
  for (const key of ["streaming", "native"] as const) {
    if (needs[key] && !targetCapabilities[target][key])
      throw new Error(`${target} adapter does not support ${key}`);
  }
}

export function cloudflare<Env = unknown>(app: App) {
  return {
    fetch(request: Request, env: Env, context: { waitUntil(task: Promise<unknown>): void }) {
      return app(request, { env, waitUntil: (task) => context.waitUntil(task), platform: context });
    },
  };
}

/** HTTP API payload v2 and Function URLs only; REST API v1 and ALB are rejected. */
export interface LambdaHttpEvent {
  version: string;
  rawPath: string;
  rawQueryString: string;
  headers: Record<string, string | undefined>;
  cookies?: string[];
  body?: string | null;
  isBase64Encoded?: boolean;
  requestContext: { domainName: string; http: { method: string } };
}
export function lambda(app: App, options: { maxResponseBytes?: number } = {}) {
  return async (event: LambdaHttpEvent, context?: unknown) => {
    if (event.version !== "2.0")
      throw new Error("lambda adapter requires HTTP payload version 2.0");
    const headers = new Headers();
    for (const [name, value] of Object.entries(event.headers))
      if (value !== undefined) headers.set(name, value);
    if (event.cookies?.length) headers.set("cookie", event.cookies.join("; "));
    const method = event.requestContext.http.method;
    // The origin comes from gateway metadata, not an untrusted Host header.
    const url = `https://${event.requestContext.domainName}${event.rawPath}${event.rawQueryString ? `?${event.rawQueryString}` : ""}`;
    const body =
      event.body == null ? undefined : event.isBase64Encoded ? decode(event.body) : event.body;
    const response = await app(
      new Request(url, {
        method,
        headers,
        body: ["GET", "HEAD"].includes(method) ? undefined : body,
      }),
      { platform: context },
    );
    const bytes = await buffered(response, options.maxResponseBytes ?? 4 * 1024 * 1024);
    const resultHeaders: Record<string, string> = {};
    response.headers.forEach((value, name) => {
      if (!framing(name) && name !== "set-cookie") resultHeaders[name] = value;
    });
    return {
      statusCode: response.status,
      headers: resultHeaders,
      cookies: response.headers.getSetCookie(),
      body: encode(bytes),
      isBase64Encoded: true,
    };
  };
}

export interface GoogleRequest {
  method: string;
  originalUrl: string;
  headers: Record<string, string | string[] | undefined>;
  rawBody?: Uint8Array;
}
export interface GoogleResponse {
  status(code: number): unknown;
  setHeader(name: string, value: string | string[]): unknown;
  end(body: Uint8Array): unknown;
}
/** Register the returned handler with @google-cloud/functions-framework.http(). */
export function google(app: App, options: { origin: string; maxResponseBytes?: number }) {
  return async (input: GoogleRequest, output: GoogleResponse) => {
    const headers = new Headers();
    for (const [key, value] of Object.entries(input.headers)) {
      for (const item of Array.isArray(value) ? value : value === undefined ? [] : [value])
        headers.append(key, item);
    }
    const response = await app(
      new Request(new URL(input.originalUrl, options.origin), {
        method: input.method,
        headers,
        body: ["GET", "HEAD"].includes(input.method)
          ? undefined
          : input.rawBody
            ? new Uint8Array(input.rawBody)
            : undefined,
      }),
      { platform: input },
    );
    const body = await buffered(response, options.maxResponseBytes ?? 4 * 1024 * 1024);
    output.status(response.status);
    response.headers.forEach((value, name) => {
      if (!framing(name) && name !== "set-cookie") output.setHeader(name, value);
    });
    const cookies = response.headers.getSetCookie();
    if (cookies.length) output.setHeader("set-cookie", cookies);
    output.end(body);
  };
}

export interface AzureRequest {
  url: string;
  method: string;
  headers: Iterable<[string, string]>;
  arrayBuffer(): Promise<ArrayBuffer>;
}
/** Azure Functions v4 HttpResponseInit; repeat cookies via header tuples. */
export function azure(app: App, options: { maxResponseBytes?: number } = {}) {
  return async (input: AzureRequest, context?: unknown) => {
    const response = await app(
      new Request(input.url, {
        method: input.method,
        headers: [...input.headers],
        body: ["GET", "HEAD"].includes(input.method) ? undefined : await input.arrayBuffer(),
      }),
      { platform: context },
    );
    const body = await buffered(response, options.maxResponseBytes ?? 4 * 1024 * 1024);
    const headers: [string, string][] = [];
    response.headers.forEach((value, name) => {
      if (!framing(name) && name !== "set-cookie") headers.push([name, value]);
    });
    for (const cookie of response.headers.getSetCookie()) headers.push(["set-cookie", cookie]);
    return { status: response.status, headers, body };
  };
}
function framing(name: string): boolean {
  return ["content-length", "transfer-encoding", "connection"].includes(name);
}
async function buffered(response: Response, limit: number): Promise<Uint8Array> {
  if (!Number.isSafeInteger(limit) || limit < 0)
    throw new Error("maxResponseBytes must be a nonnegative safe integer");
  const reader = response.body?.getReader();
  if (!reader) return new Uint8Array();
  const chunks: Uint8Array[] = [];
  let size = 0;
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > limit) {
        void reader.cancel().catch(() => {});
        throw new Error("response exceeds adapter maxResponseBytes");
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  const result = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) {
    result.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return result;
}
function decode(value: string): Uint8Array<ArrayBuffer> {
  return Uint8Array.from(atob(value), (c) => c.charCodeAt(0));
}
function encode(value: Uint8Array): string {
  let binary = "";
  for (let i = 0; i < value.length; i += 8192)
    binary += String.fromCharCode(...value.subarray(i, i + 8192));
  return btoa(binary);
}
