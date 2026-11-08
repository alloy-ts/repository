import * as dist from "../../../dist/index.js";

// Keep existing git exports while adding the unambiguous Object namespace.
export const Object = globalThis.Object.assign(dist.Object, {
  Blob: dist.object.GitBlob,
  Commit: dist.object.Commit,
  Tree: dist.object.Tree,
  Tag: dist.object.Tag,
});
export type Object = dist.Object;
export const { Commit, Tree, Tag, GitBlob } = dist.object;
export type Commit = dist.object.Commit;
export type Tree = dist.object.Tree;
export type Tag = dist.object.Tag;
export type GitBlob = dist.object.GitBlob;
export const object = { ...dist.object, Blob: dist.object.GitBlob };
export * from "../../../dist/index.js";
export { Request, Response, Headers, fetch } from "./net/request.ts";
export type { RequestOpts, RequestInit, ResponseInit } from "./net/request.ts";
export { Server } from "./net/server.ts";
export type { ServerOptions } from "./net/server.ts";
export { File, Blob } from "./fs/file.ts";
export const Worker = dist.Task;
export type Worker = dist.Task;
// Node's adapters preserve EventEmitter, stream, callback, and error semantics.
export * as net from "node:net";
export * as http from "node:http";
export * as fs from "node:fs";
export {
  Socket,
  Server as NetServer,
  createServer as createNetServer,
  connect as connectSocket,
} from "node:net";
export {
  IncomingMessage,
  ServerResponse,
  Server as HttpServer,
  createServer as createHttpServer,
} from "node:http";
