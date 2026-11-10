import type { Socket } from "node:net";

/**
 * HTTP/1 listener for `Server.start()`.
 *
 * Rocket 0.5's `Method` is a closed enum (Get, Put, Post, Delete, Options,
 * Head, Trace, Connect, Patch). `Route::new` cannot register QUERY, PURGE, or
 * the arbitrary methods accepted by `server.on`. Node's `http` parser has the
 * same kind of allow-list (`http.METHODS`): it includes QUERY and PURGE, and
 * rejects other tokens before user code runs. This module is the only place
 * that reads those bytes. `Server.start()` calls `attachHttpSocket` and then
 * dispatches through `server.fetch()`; it does not implement HTTP itself.
 * `RocketServer` remains exported for callers that want the native listener.
 */

/** Bytes of a request body, not counting the header block. */
export const MAX_HTTP_BODY = 16 * 1024 * 1024;

/** Request header block, excluding the blank line that ends it. */
export const MAX_HTTP_HEADERS = 65_536;

export interface HttpRequestMessage {
  method: string;
  target: string;
  version: string;
  headers: [string, string][];
  body?: Buffer;
  keepAlive: boolean;
}

interface Preface {
  method: string;
  target: string;
  version: string;
  headers: [string, string][];
  keepAlive: boolean;
  expectsContinue: boolean;
  body: { kind: "none" } | { kind: "length"; length: number } | { kind: "chunked" };
}

export type HttpEvent =
  | { type: "continue" }
  | { type: "request"; message: HttpRequestMessage }
  | { type: "error"; status: number; reason: string };

/**
 * Incremental HTTP/1 parser used by `Server.start()`.
 * One session may hold pipelined bytes. See the module comment for why this
 * parser exists instead of Rocket or `node:http`.
 */
export class HttpSession {
  #buffer = Buffer.alloc(0);
  #preface?: Preface;
  #continued = false;

  push(chunk: Buffer): HttpEvent[] {
    if (chunk.length > 0) this.#buffer = Buffer.concat([this.#buffer, chunk]);
    const events: HttpEvent[] = [];
    while (true) {
      if (!this.#preface) {
        const scanned = scanHeaders(this.#buffer);
        if (scanned.kind === "partial") break;
        if (scanned.kind === "error") {
          events.push({ type: "error", status: scanned.status, reason: scanned.reason });
          this.#buffer = Buffer.alloc(0);
          return events;
        }
        this.#preface = scanned.preface;
        this.#buffer = this.#buffer.subarray(scanned.headerEnd);
        this.#continued = false;
      }
      if (this.#preface.expectsContinue && !this.#continued && this.#preface.body.kind !== "none") {
        this.#continued = true;
        events.push({ type: "continue" });
      }
      const body = takeBody(this.#buffer, this.#preface.body);
      if (body.kind === "partial") break;
      if (body.kind === "error") {
        events.push({ type: "error", status: body.status, reason: body.reason });
        this.#preface = undefined;
        this.#buffer = Buffer.alloc(0);
        return events;
      }
      const preface = this.#preface;
      this.#buffer = this.#buffer.subarray(body.consumed);
      this.#preface = undefined;
      events.push({
        type: "request",
        message: {
          method: preface.method,
          target: preface.target,
          version: preface.version,
          headers: preface.headers,
          body: body.data,
          keepAlive: preface.keepAlive,
        },
      });
      if (!preface.keepAlive) break;
    }
    return events;
  }
}

export function encodeHttpResponse(
  status: number,
  statusText: string,
  headers: Iterable<[string, string]>,
  body: Uint8Array,
  keepAlive: boolean,
  head: boolean,
): Buffer {
  const payload = head ? Buffer.alloc(0) : Buffer.from(body);
  const declared = head
    ? [...headers].find(([key]) => key.toLowerCase() === "content-length")?.[1]
    : undefined;
  const length = declared !== undefined && /^\d+$/.test(declared) ? Number(declared) : body.byteLength;
  const reason = statusText || "OK";
  const lines = [
    `HTTP/1.1 ${status} ${reason}`,
    `connection: ${keepAlive ? "keep-alive" : "close"}`,
    `content-length: ${head ? length : payload.length}`,
  ];
  for (const [key, value] of headers) {
    const name = key.toLowerCase();
    if (name === "content-length" || name === "transfer-encoding" || name === "connection") continue;
    lines.push(`${key}: ${value}`);
  }
  return Buffer.concat([Buffer.from(`${lines.join("\r\n")}\r\n\r\n`), payload]);
}

/** Read one connection. `handle` runs for each complete request, in order. */
export function attachHttpSocket(
  socket: Socket,
  handle: (message: HttpRequestMessage) => Promise<{
    status: number;
    statusText: string;
    headers: Iterable<[string, string]>;
    body: Uint8Array;
  }>,
): void {
  const session = new HttpSession();
  let closed = false;
  let chain = Promise.resolve();
  const fail = (status: number, reason: string) => {
    if (closed || socket.destroyed) return;
    closed = true;
    socket.end(encodeHttpResponse(status, reason, [], Buffer.from(reason), false, false));
  };
  socket.on("data", (chunk: Buffer) => {
    chain = chain
      .then(async () => {
        if (closed) return;
        for (const event of session.push(chunk)) {
          if (closed) return;
          if (event.type === "continue") {
            socket.write("HTTP/1.1 100 Continue\r\n\r\n");
            continue;
          }
          if (event.type === "error") {
            fail(event.status, event.reason);
            return;
          }
          const result = await handle(event.message);
          const bytes = encodeHttpResponse(
            result.status,
            result.statusText,
            result.headers,
            result.body,
            event.message.keepAlive,
            event.message.method === "HEAD",
          );
          if (!event.message.keepAlive) {
            closed = true;
            socket.end(bytes);
            return;
          }
          socket.write(bytes);
        }
      })
      .catch(() => fail(500, "Internal Server Error"));
  });
  socket.on("error", () => socket.destroy());
}

function scanHeaders(buffer: Buffer):
  | { kind: "partial" }
  | { kind: "error"; status: number; reason: string }
  | { kind: "ok"; preface: Preface; headerEnd: number } {
  const headerEnd = buffer.indexOf("\r\n\r\n");
  if (headerEnd < 0) {
    if (buffer.length > MAX_HTTP_HEADERS) {
      return { kind: "error", status: 431, reason: "Request Header Fields Too Large" };
    }
    return { kind: "partial" };
  }
  // A complete block can arrive in one chunk, so the limit also applies after
  // the terminator is found. `headerEnd` is the header bytes before `\r\n\r\n`.
  if (headerEnd > MAX_HTTP_HEADERS) {
    return { kind: "error", status: 431, reason: "Request Header Fields Too Large" };
  }
  const header = buffer.subarray(0, headerEnd).toString("latin1");
  const [start, ...lines] = header.split("\r\n");
  const parsed = /^([!#$%&'*+\-.^_`|~0-9A-Za-z]+)[ \t]+(\S+)[ \t]+HTTP\/(\d(?:\.\d)?)$/.exec(start ?? "");
  if (!parsed?.[1] || !parsed[2] || !parsed[3]) return { kind: "error", status: 400, reason: "Bad Request" };
  const headers: [string, string][] = [];
  let length: number | undefined;
  let chunked = false;
  let otherEncoding = false;
  let connection: string | undefined;
  let expectsContinue = false;
  for (const line of lines) {
    if (line.length === 0) return { kind: "error", status: 400, reason: "Bad Request" };
    const split = line.indexOf(":");
    if (split <= 0) return { kind: "error", status: 400, reason: "Bad Request" };
    const name = line.slice(0, split).trim();
    const value = line.slice(split + 1).trim();
    headers.push([name, value]);
    const lower = name.toLowerCase();
    if (lower === "content-length") {
      if (!/^\d+$/.test(value)) return { kind: "error", status: 400, reason: "Bad Request" };
      const next = Number(value);
      if (length !== undefined && length !== next) return { kind: "error", status: 400, reason: "Bad Request" };
      length = next;
    } else if (lower === "transfer-encoding") {
      const codings = value.split(",").map((part) => part.trim().toLowerCase()).filter(Boolean);
      if (codings.length === 0 || codings.some((coding) => coding.split(";")[0] !== "chunked")) {
        otherEncoding = true;
      }
      if (codings.at(-1)?.split(";")[0] === "chunked") chunked = true;
    } else if (lower === "connection") connection = value.toLowerCase();
    else if (lower === "expect" && value.toLowerCase() === "100-continue") expectsContinue = true;
  }
  if (otherEncoding) return { kind: "error", status: 501, reason: "Not Implemented" };
  if (chunked && length !== undefined) return { kind: "error", status: 400, reason: "Bad Request" };
  if (length !== undefined && length > MAX_HTTP_BODY) return { kind: "error", status: 413, reason: "Payload Too Large" };
  const version = parsed[3];
  const keepAlive = connection?.includes("close")
    ? false
    : connection?.includes("keep-alive")
      ? true
      : version !== "1.0";
  const body = chunked
    ? ({ kind: "chunked" } as const)
    : length && length > 0
      ? ({ kind: "length", length } as const)
      : ({ kind: "none" } as const);
  return {
    kind: "ok",
    headerEnd: headerEnd + 4,
    preface: {
      method: parsed[1],
      target: parsed[2],
      version,
      headers,
      keepAlive,
      expectsContinue,
      body,
    },
  };
}

function takeBody(
  buffer: Buffer,
  body: Preface["body"],
):
  | { kind: "partial" }
  | { kind: "error"; status: number; reason: string }
  | { kind: "complete"; data?: Buffer; consumed: number } {
  if (body.kind === "none") return { kind: "complete", consumed: 0 };
  if (body.kind === "length") {
    if (buffer.length < body.length) return { kind: "partial" };
    return { kind: "complete", data: Buffer.from(buffer.subarray(0, body.length)), consumed: body.length };
  }
  return decodeChunked(buffer);
}

function decodeChunked(buffer: Buffer):
  | { kind: "partial" }
  | { kind: "error"; status: number; reason: string }
  | { kind: "complete"; data: Buffer; consumed: number } {
  const parts: Buffer[] = [];
  let offset = 0;
  let sizeTotal = 0;
  while (true) {
    const lineEnd = buffer.indexOf("\r\n", offset);
    if (lineEnd < 0) return { kind: "partial" };
    const sizeText = buffer.subarray(offset, lineEnd).toString("latin1").split(";")[0]?.trim() ?? "";
    if (!/^[0-9a-fA-F]+$/.test(sizeText)) return { kind: "error", status: 400, reason: "Bad Request" };
    const size = Number.parseInt(sizeText, 16);
    const dataStart = lineEnd + 2;
    if (size === 0) {
      const trailerEnd = buffer.indexOf("\r\n\r\n", dataStart - 2);
      if (trailerEnd < 0) return { kind: "partial" };
      return { kind: "complete", data: Buffer.concat(parts), consumed: trailerEnd + 4 };
    }
    if (size > MAX_HTTP_BODY || sizeTotal + size > MAX_HTTP_BODY) {
      return { kind: "error", status: 413, reason: "Payload Too Large" };
    }
    if (buffer.length < dataStart + size + 2) return { kind: "partial" };
    if (buffer[dataStart + size] !== 13 || buffer[dataStart + size + 1] !== 10) {
      return { kind: "error", status: 400, reason: "Bad Request" };
    }
    parts.push(Buffer.from(buffer.subarray(dataStart, dataStart + size)));
    sizeTotal += size;
    offset = dataStart + size + 2;
  }
}
