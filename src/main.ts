import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { existsSync } from "node:fs";

const require = createRequire(import.meta.url);
const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

// In dev / test, native binary is directly in root or in dist/index.js if built by napi
const localDist = join(__dirname, "../dist/index.js");
const localRoot = join(__dirname, "../index.js");
const bindingPath = existsSync(localDist) ? localDist : localRoot;

const binding = require(bindingPath);

export interface Signature {
  name: string;
  email: string;
  time: number;
  timezoneOffset: number;
}

export interface SignatureVerification {
  signed: boolean;
  format?: string;
  status?: string;
  trust?: string;
  signer?: string;
  key?: string;
  fingerprint?: string;
  valid?: boolean;
  output?: string;
}

export interface LogEntry {
  id: string;
  message: string;
  subject: string;
  authorName: string;
  authorEmail: string;
  authorTime: number;
  committerName: string;
  committerEmail: string;
  committerTime: number;
  parentIds: Array<string>;
  treeId: string;
}

export interface Change {
  path: string;
  oldId?: string;
  newId?: string;
  kind: string;
}

export interface TreeEntry {
  name: string;
  id: string;
  mode: string;
}

export interface RemoteFetchOutcome {
  updatedRefs: Array<string>;
}

export type Blob = any;
export type Commit = any;
export type GitObject = any;
export type Head = any;
export type Reference = any;
export type Remote = any;
export type Repository = any;
export type Tag = any;
export type Tree = any;

export const {
  Blob,
  Commit,
  GitObject,
  Head,
  Reference,
  Remote,
  Repository,
  Tag,
  Tree,
  add,
  setupRepositoryFromManifest,
} = binding;

export default binding;
