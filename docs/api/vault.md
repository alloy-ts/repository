# Vault & KvVault API

File-backed encrypted vault management, key-value storage, and git-crypt key management bindings.

`Vault` provides encrypted secret storage backed by local JSON files with Argon2id KDF, XChaCha20-Poly1305 AEAD encryption, versioning, rotation policies, and structure-bound MAC verification (`v3` schema).

`KvVault` provides a synchronous local key-value store backed by `fjall` for persisting opaque string blobs (such as encrypted vault JSON states) without external network dependencies.

`KeyManager` handles git-crypt symmetric key generation, storage, and export.

---

## `Vault`

### Constructors & Opening

| Method | Signature | Notes |
| --- | --- | --- |
| `constructor` | `(path: string, password: Buffer) => Vault` | Creates a new empty vault at `path` encrypted with `password` and saves it. |
| `open` | `(path: string, password: Buffer, readOnly?: boolean) => Vault` | Opens an existing encrypted vault file. Set `readOnly: true` to prevent mutations. |
| `openWithKey` | `(path: string, key: Buffer, readOnly?: boolean) => Vault` | Opens an existing vault using a raw 32-byte key (e.g. unwrapped DEK). |

### Secret Operations

| Method | Signature | Notes |
| --- | --- | --- |
| `get` | `(key: string) => string` | Decrypts and returns the plaintext value of `key`. Throws if missing. |
| `set` | `(key: string, value: string, tags?: Record<string, string>) => void` | Inserts or updates a secret in memory. Previous values are pushed into version history. |
| `delete` | `(key: string) => void` | Removes a secret and updates the vault timestamp. |
| `list` | `() => string[]` | Returns all secret key names in sorted order. |
| `renameKey` | `(oldKey: string, newKey: string, overwrite?: boolean) => void` | Moves a secret and its version history to a new key name, re-binding its AEAD tag. |
| `rotate` | `(newPassword: Buffer) => void` | Re-encrypts all live secrets and historical entries under `newPassword`. |
| `exportAll` | `() => Record<string, string>` | Decrypts and returns a plain map of all current secret key-value pairs. |
| `secretCount` | `() => number` | Returns the total count of live secret entries. |
| `save` | `() => void` | Persists in-memory vault mutations atomically to disk. |

### Version History & Rotation Policies

| Method | Signature | Notes |
| --- | --- | --- |
| `history` | `(key: string) => VaultHistoryEntry[]` | Returns version entries (`[{ version, timestamp }]`), newest-first (version `0` = current). |
| `getVersion` | `(key: string, version: number) => string` | Decrypts and returns a specific historical version of a secret. |
| `revertToVersion` | `(key: string, version: number) => void` | Promotes historical `version` to current live value and demotes live value to history. |
| `pruneHistory` | `(key: string, keepN: number) => void` | Prunes historical entries for `key` to keep at most `keepN` versions. |
| `rotationDue` | `() => VaultRotationDue[]` | Returns secrets overdue for rotation based on `rotate_policy` tag (e.g. `"90d"`). |
| `parseRotationDays` | `(policy: string) => number \| null` | Static helper that parses duration policy strings (e.g., `"90d"` -> `90`). |

### Status & Management

| Method | Signature | Notes |
| --- | --- | --- |
| `path` | `() => string` | Returns the on-disk file path of the opened vault. |
| `accessProfile` | `() => VaultAccessProfile` | Returns `"ReadWrite"` or `"ReadOnly"`. |
| `structureBinding` | `() => VaultStructureBinding` | Returns `"Bound"` (`v3` schema) or `"Legacy"` (`v1`/`v2`). |
| `rollbackSuspected` | `() => boolean` | Returns `true` if file version counter went backwards relative to high-water mark. |
| `isTeamVault` | `(path: string) => boolean` | Static helper checking if `path` contains age recipients and wrapped DEK. |
| `close` | `() => void` | Explicitly releases the advisory file lock guard (`.vault.lock`). |

---

## `KvVault`

Synchronous, local key-value store backed by `fjall` for storing opaque string blobs.

| Method | Signature | Notes |
| --- | --- | --- |
| `constructor` | `(path: string) => KvVault` | Opens or creates a local `fjall` keyspace store at `path`. |
| `put` | `(key: string, value: string) => void` | Stores `value` under `key`. |
| `get` | `(key: string) => string \| null` | Fetches `value` under `key`, or `null` if absent. |
| `delete` | `(key: string) => void` | Deletes `key` from the store. |
| `list` | `(prefix: string) => string[]` | Lists sorted key names matching `prefix` (empty prefix `""` lists all keys). |

---

## `KeyManager`

Symmetric key manager for `git-crypt`.

| Method | Signature | Notes |
| --- | --- | --- |
| `constructor` | `(gitDir: string) => KeyManager` | Initializes manager for the repository at `gitDir`. |
| `generateKey` | `() => Buffer` | Generates a fresh random symmetric key and persists it. |
| `loadKey` | `() => Buffer` | Loads default key from disk. |
| `saveKey` | `(key: Buffer) => void` | Persists symmetric key bytes to default path with `0600` permissions. |
| `importKey` | `(inputPath: string) => void` | Imports symmetric key file as default key. |
| `exportKey` | `(outputPath: string) => void` | Exports default key file to `outputPath`. |
| `defaultKeyPath` | `() => string` | Returns configured or default symmetric key path (`.git/crypt/keys/default`). |
| `initDirs` | `() => void` | Creates `.git/crypt` directory structure. |
| `isInitialized` | `() => boolean` | Returns `true` if `.git/crypt` directory exists. |

---

## NAPI Types & Enums

```ts
export declare const enum VaultStructureBinding {
  Legacy = "Legacy",
  Bound = "Bound",
}

export declare const enum VaultAccessProfile {
  ReadWrite = "ReadWrite",
  ReadOnly = "ReadOnly",
}

export interface VaultHistoryEntry {
  version: number;
  timestamp: string; // RFC3339 format
}

export interface VaultRotationDue {
  key: string;
  overdue_days: number;
  policy: string;
}
```
