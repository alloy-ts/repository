/** Web body/header semantics live in the JS runtime; HTTP I/O stays in http_request. */
import * as native from "../../../../dist/index.js";
import { Response, type ResponseInit } from "./response.ts";

export { Response, type ResponseInit };
export const Headers = globalThis.Headers;
export type RequestOpts = globalThis.RequestInit & { type?: string };
export type RequestInit = globalThis.RequestInit;

export class Request extends globalThis.Request {
  start(): Promise<Response> {
    return fetch(this);
  }
  override clone(): Request {
    return new Request(super.clone());
  }
}

export async function fetch(
  input: string | URL | globalThis.Request,
  init?: RequestInit,
): Promise<Response> {
  const request = new Request(input, init);
  request.signal.throwIfAborted();
  const body = request.body ? Buffer.from(await request.arrayBuffer()) : undefined;
  request.signal.throwIfAborted();
  const flag = new native.AbortFlag();
  if (request.signal.aborted) flag.abort();
  else request.signal.addEventListener("abort", () => flag.abort(), { once: true });
  const operation = native
    .fetch(
      request.url,
      {
        method: request.method,
        headers: Object.fromEntries(request.headers),
        body,
        redirect: request.redirect,
      },
      flag,
    )
    .then(async (result) => {
      const bytes = await result.bytes();
      const response = new Response(
        request.method === "HEAD" || [204, 205, 304].includes(result.status) ? null : bytes,
        { status: result.status, statusText: result.statusText, headers: result.headers },
      );
      return withMetadata(response, result.url, result.redirected);
    });
  return new Promise((resolve, reject) => {
    const abort = () => reject(request.signal.reason);
    request.signal.addEventListener("abort", abort, { once: true });
    operation
      .then(resolve, reject)
      .finally(() => request.signal.removeEventListener("abort", abort));
    if (request.signal.aborted) abort();
  });
}

function withMetadata(response: Response, url: string, redirected: boolean): Response {
  const clone = response.clone.bind(response);
  Object.defineProperties(response, {
    url: { value: url },
    redirected: { value: redirected },
    type: { value: "basic" },
    clone: { value: () => withMetadata(clone(), url, redirected) },
  });
  return response;
}
