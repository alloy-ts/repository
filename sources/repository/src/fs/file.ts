import { createReadStream, statSync } from "node:fs";
import { Readable } from "node:stream";
import { fileURLToPath } from "node:url";
import { basename } from "node:path";
import * as native from "../../../../dist/index.js";
import type { RequestOpts } from "../net/request.ts";

export const Blob = globalThis.Blob;
export type Blob = globalThis.Blob;

/** A lazy disk file, extending the Web Blob surface with Bun-style disk operations. */
export class File extends globalThis.File {
  readonly #path: string;
  readonly #native: InstanceType<typeof native.File>;
  readonly #type: string;
  constructor(urlOrPath: string | URL, options: RequestOpts = {}) {
    super([], "");
    this.#path =
      urlOrPath instanceof URL || urlOrPath.startsWith("file:")
        ? fileURLToPath(urlOrPath)
        : urlOrPath;
    this.#type = new Blob([], {
      type:
        options.type ??
        new Headers(options.headers).get("content-type") ??
        "text/plain;charset=utf-8",
    }).type;
    this.#native = new native.File(this.#path);
  }
  override get name(): string {
    return basename(this.#path);
  }
  override get lastModified(): number {
    return Math.floor(this.#stat()?.mtimeMs ?? 0);
  }
  override get webkitRelativePath(): string {
    return "";
  }
  override get size(): number {
    return this.#stat()?.size ?? 0;
  }
  override get type(): string {
    return this.#type;
  }
  #stat() {
    try {
      return statSync(this.#path);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return;
      throw error;
    }
  }
  exists(): Promise<boolean> {
    return this.#native.exists();
  }
  exist(): Promise<boolean> {
    return this.exists();
  }
  write(data: string | Uint8Array | ArrayBuffer): Promise<number> {
    return this.#native.write(
      typeof data === "string"
        ? data
        : Buffer.from(data instanceof ArrayBuffer ? new Uint8Array(data) : data),
    );
  }
  delete(options?: { recursive?: boolean }): void {
    this.#native.delete(options);
  }
  override async bytes(): Promise<Uint8Array<ArrayBuffer>> {
    return new Uint8Array(await this.#native.bytes());
  }
  override async arrayBuffer(): Promise<ArrayBuffer> {
    return (await this.bytes()).buffer;
  }
  override async text(): Promise<string> {
    return new TextDecoder().decode(await this.bytes());
  }
  async json(): Promise<unknown> {
    return JSON.parse(await this.text());
  }
  override stream(): ReadableStream<Uint8Array<ArrayBuffer>> {
    return Readable.toWeb(createReadStream(this.#path)) as ReadableStream<Uint8Array<ArrayBuffer>>;
  }
  override slice(start?: number, end?: number, contentType = ""): Blob {
    // A slice is a snapshot; opening the source File itself remains lazy.
    return new Blob([new Uint8Array(native.fsReadFileSync(this.#path))]).slice(
      start,
      end,
      contentType,
    );
  }
}
