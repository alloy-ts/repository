import * as dist from "../../../dist/index.js";

// Keep existing git exports while adding the unambiguous Object and Repository namespaces.
const repoNs = dist.Repository;
const RepositoryClass = repoNs.Repository;

// Make Repository namespace properties accessible directly on Repository class
// (e.g. Repository.Manifest, Repository.open, Repository.Index, etc.)
export const Repository = Object.assign(RepositoryClass, repoNs);
export type Repository = InstanceType<typeof RepositoryClass>;

export const {
  Commit,
  Tree,
  Tag,
  GitBlob,
  Manifest,
  Index,
  Reference,
  Remote,
  Worktree,
  Cloner,
  Trailer,
  Reflog,
  Rebase,
  Patch,
  Pack,
  Metrics,
  Mailmap,
  Importer,
  Auth,
  AttributeStack,
  ConfigTransaction,
  ConfigSnapshot,
  Workspace,
  WorkspaceLoader,
  LockedWorkspace,
  StoreFactories,
  TreeBuilder,
  TreeEntry,
  Revwalk,
  Submodule,
  ModulesFile,
  FilterPipeline,
  Pathspec,
  RefSpec,
  MatchGroup,
  RefStore,
  ReferenceIterator,
  RefLock,
  Head,
  Operation,
  OperationId,
  RemoteRef,
  RemoteView,
  View,
  ViewId,
  Oid,
  PackEntry,
  PackEntryHeader,
  ChunkIndex,
  IndexEntryMode,
  IndexFsMetadata,
  RemoteProgress,
  Graph,
  TraversalGraph,
} = repoNs;

export const RepositoryManifest = Manifest;
export type RepositoryManifest = typeof Manifest;
export type Manifest = InstanceType<typeof Manifest>;

export type Commit = dist.Repository.Commit;
export type Tree = dist.Repository.Tree;
export type Tag = dist.Repository.Tag;
export type GitBlob = dist.Repository.GitBlob;

export const object = {
  Commit: repoNs.Commit,
  Tree: repoNs.Tree,
  Tag: repoNs.Tag,
  Blob: repoNs.GitBlob,
  GitBlob: repoNs.GitBlob,
};

export const Object_ = globalThis.Object.assign(dist.Object ?? {}, {
  Blob: repoNs.GitBlob,
  Commit: repoNs.Commit,
  Tree: repoNs.Tree,
  Tag: repoNs.Tag,
});
export { Object_ as Object };

export * from "../../../dist/index.js";
export { Request, Response, Headers, fetch } from "./net/request.ts";
export type { RequestOpts, RequestInit, ResponseInit } from "./net/request.ts";
export { Server } from "./net/server.ts";
export type { ServerOptions } from "./net/server.ts";
export { File, Blob } from "./fs/file.ts";
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
