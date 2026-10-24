# Vault API

The `Vault` module provides authenticated secret storage, key management, local embedded key-value storage, and diagnostic utilities.

## File-backed Vault (`Vault`)

`Vault` manages encrypted, versioned secrets stored in a single JSON file. Updates are written atomically with owner-only file permissions (`0600`) and process-aware advisory locking (`.vault.lock`).

### Classes and Types

#### `Vault`

JS handle to an open encrypted vault file.

```ts
export class Vault {
  /** Create a new encrypted vault file. */
  constructor(path: string, password: Buffer);

  /** Open an existing encrypted vault file. */
  static open(path: string, password: Buffer, readOnly?: boolean): Vault;

  /** Open a vault with a pre-derived 32-byte key. */
  static openWithKey(path: string, key: Buffer, readOnly?: boolean): Vault;

  /** Check if a vault on disk is a team vault (uses age recipients). */
  static isTeamVault(path: string): boolean;

  /** Parse a duration policy string (e.g. "90d") into days, or null if invalid. */
  static parseRotationDays(policy: string): number | null;

  /** Insert or update a secret value with optional tags. */
  set(key: string, value: string, tags?: Record<string, string>): void;

  /** Decrypt and return the live secret value. */
  get(key: string): string;

  /** Remove a secret from the vault. */
  delete(key: string): void;

  /** List all secret key names in sorted order. */
  list(): string[];

  /** Rename a secret key. */
  renameKey(oldKey: string, newKey: string, overwrite?: boolean): void;

  /** Re-encrypt all secrets under a new master password. */
  rotate(newPassword: Buffer): void;

  /** Total number of secrets stored in the vault. */
  secretCount(): number;

  /** Persist buffered changes to disk. */
  save(): void;

  /** Decrypt a specific historical version of a secret (0 = current). */
  getVersion(key: string, version: number): string;

  /** Get version history metadata for a secret. */
  history(key: string): VaultHistoryEntry[];

  /** Revert a secret to a previous version. */
  revertToVersion(key: string, version: number): void;

  /** Prune historical versions of a secret down to keepN entries. */
  pruneHistory(key: string, keepN: number): void;

  /** Decrypt and export all secrets as a key-value map. */
  exportAll(): Record<string, string>;

  /** On-disk path of this vault. */
  path(): string;

  /** Access profile (`ReadWrite` or `ReadOnly`). */
  accessProfile(): VaultAccessProfile;

  /** Structure binding level (`Legacy` or `Bound`). */
  structureBinding(): VaultStructureBinding;

  /** Returns true if a write counter rollback was detected on open. */
  rollbackSuspected(): boolean;

  /** Returns entries whose `rotate_policy` tag is overdue. */
  rotationDue(): VaultRotationDue[];
}
```

#### Supporting Types

```ts
export enum VaultAccessProfile {
  ReadWrite = "ReadWrite",
  ReadOnly = "ReadOnly",
}

export enum VaultStructureBinding {
  Legacy = "Legacy",
  Bound = "Bound",
}

export interface VaultHistoryEntry {
  version: number;
  timestamp: string; // RFC3339 timestamp
}

export interface VaultRotationDue {
  key: string;
  overdueDays: number;
  policy: string;
}
```

---

## Embedded KV Store (`KvVault`)

`KvVault` is a synchronous, local key-value store backed by `fjall`. It stores string keys and string values (opaque blobs) on disk without network or external database dependencies.

```ts
export class KvVault {
  /** Open or create a KvVault database directory at path. */
  constructor(path: string);

  /** Store a value under key. */
  put(key: string, value: string): void;

  /** Fetch value for key, or null if absent. */
  get(key: string): string | null;

  /** Remove a key from the store. */
  delete(key: string): void;

  /** List keys matching prefix in sorted order. */
  list(prefix: string): string[];
}
```

---

## Symmetric Key Manager (`KeyManager`)

`KeyManager` manages repository-level symmetric key files under `<git_dir>/crypt/keys/default`.

```ts
export class KeyManager {
  /** Create a manager for a Git directory (.git). */
  constructor(gitDir: string);

  /** Generate a new 32-byte random key and persist it. */
  generateKey(): Buffer;

  /** Load the default key from disk. */
  loadKey(): Buffer;

  /** Save raw key bytes as the default key. */
  saveKey(key: Buffer): void;

  /** Export default key to an output file path. */
  exportKey(outputPath: string): void;

  /** Import a key file and save as default key. */
  importKey(inputPath: string): void;

  /** Initialize crypt directory structure (<git_dir>/crypt/keys). */
  initDirs(): void;

  /** Check if crypt directories are initialized. */
  isInitialized(): boolean;

  /** Path to the crypt directory. */
  gitCryptDir(): string;

  /** Path to the default key file. */
  defaultKeyPath(): string;
}
```

---

## Diagnostics and Utilities

Free functions for lock file inspection and schema verification:

| Function | Signature | Description |
| --- | --- | --- |
| `lockStateInfo` | `(vaultPath: string) => string \| null` | Inspects lock state for `vaultPath` (`"held_by:<pid>"` or `"unparseable"`). |
| `liveLockHolderPid` | `(vaultPath: string) => number \| null` | Returns the process ID of the live process holding the vault lock, if any. |
| `checkIsSupportedSchema` | `(schema: string) => boolean` | Checks if a vault schema identifier is supported. |
| `checkSchemaIsBound` | `(schema: string) => boolean` | Checks if a vault schema implements per-entry AAD and structural MAC binding. |

---

## Identity Manager (`IdentityManager`)

Available when built with the `ssh` feature.

```ts
export class IdentityManager {
  /** Encrypt a repository symmetric key for an SSH recipient. */
  static encryptKeyForSshRecipient(key: Buffer, recipient: string): Buffer;

  /** Decrypt a key envelope using an SSH identity private key. */
  static decryptKeyWithSshIdentity(
    encrypted: Buffer,
    identityContent: string,
    passphrase?: string,
  ): Buffer;
}
```

---

## Working Example

```ts
import { Vault, KvVault, KeyManager, lockStateInfo } from "@alloy-ts/repository";

// --- File-backed Vault Usage ---
const password = Buffer.from("master-passphrase");
const vault = new Vault("./secrets.vault", password);

vault.set("DB_PASSWORD", "super-secret", { rotate_policy: "90d" });
vault.save();

console.log(vault.get("DB_PASSWORD")); // "super-secret"
console.log(lockStateInfo("./secrets.vault")); // "held_by:<pid>"

// --- Embedded KvVault Usage ---
const kv = new KvVault("./kv_store");
kv.put("config:app", JSON.stringify({ debug: true }));
console.log(kv.get("config:app")); // '{"debug":true}'

// --- Symmetric KeyManager Usage ---
const km = new KeyManager("./.git");
if (!km.isInitialized()) {
  km.initDirs();
}
const key = km.generateKey();
console.log("Default key path:", km.defaultKeyPath());
```
