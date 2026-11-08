import { File } from "../fs/file.ts";

export type ResponseInit = globalThis.ResponseInit;

export const Response = class extends globalThis.Response {
  constructor(body?: ConstructorParameters<typeof globalThis.Response>[0], init?: ResponseInit) {
    super(body instanceof File ? body.stream() : body, init);
  }
  static override redirect(url: string | URL, status = 302): globalThis.Response {
    if (![301, 302, 303, 307, 308].includes(status))
      throw new RangeError("invalid redirect status");
    new URL(url, "http://localhost");
    return new globalThis.Response(null, { status, headers: { location: String(url) } });
  }
};
export type Response = globalThis.Response;
