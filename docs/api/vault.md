# Vault & Secret Storage API

This module provides NAPI bindings for encrypted secret management, historical secret tracking, rotation policy checks, and local embedded key–value store persistence.

---

## Overview

The vault subsystem consists of three primary JS/TS classes:
1. **`Vault`**: File-backed encrypted vault container using Argon2id KDF and XChaCha20Poly1305 / AES AEAD ciphers.
2. **`KvVault`**: Synchronous local key–value store backed by `fjall` for persisting opaque string/blob payloads.
3. **`KeyManager`**: File-based key generator and manager for repository symmetric crypto keys.

---

## `Vault`

An open encrypted vault handle. All instance methods lock internally so the handle can be shared safely across asynchronous JS operations.

### Static Methods

| Method | Signature | Description |
| --- | --- | --- |
| `isTeamVault` | `(path: string) => boolean` | Checks whether the vault at `path` contains `age` recipients. |
| `parseRotationDays` | `(policy: string) => number \| null` | Parses a rotation policy string like `"90d"` into number of days. |

### Constructor & Open Methods

| Method / Constructor | Signature | Description |
| --- | --- | --- |
| `new Vault(path, password)` | `(path: string, password: Buffer) => Vault` | Creates a new encrypted vault at `path` using `password`. |
| `Vault.open` | `(path: string, password: Buffer, readOnly?: boolean) => Vault` | Opens an existing vault file. |
| `Vault.openWithKey` | `(path: string, key: Buffer, readOnly?: boolean) => Vault` | Opens an existing vault using a raw 32-byte key (`Buffer`). |

### Secret Operations

| Method | Signature | Description |
| --- | --- | --- |
| `set` | `(key: string, value: string, tags?: Record<string, string>) => void` | Inserts or updates a secret in memory with optional string tags. |
| `get` | `(key: string) => string` | Decrypts and returns a secret's current plaintext value. |
| `delete` | `(key: string) => void` | Removes a secret entry. |
| `list` | `() => string[]` | Returns a sorted array of secret keys stored in the vault. |
| `renameKey` | `(oldKey: string, newKey: string, overwrite?: boolean) => void` | Renames a secret key. |
| `secretCount` | `() => number` | Returns the total number of secret entries. |
| `save` | `() => void` | Flushes in-memory mutations (`set`, `delete`, `rotate`) to disk. |
| `rotate` | `(newPassword: Buffer) => void` | Re-encrypts the vault under a new master password. |

### History & Versioning

| Method | Signature | Description |
| --- | --- | --- |
| `getVersion` | `(key: string, version: number) => string` | Decrypts a specific historical version (`0` = current). |
| `history` | `(key: string) => VaultHistoryEntry[]` | Lists version metadata for a secret. |
| `revertToVersion` | `(key: string, version: number) => void` | Reverts a secret to a previous version index. |
| `pruneHistory` | `(key: string, keepN: number) => void` | Prunes historical versions keeping only the newest `keepN`. |

### Inspection & Export

| Method | Signature | Description |
| --- | --- | --- |
| `exportAll` | `() => Record<string, string>` | Decrypts and exports all current secrets as a key-value object. |
| `path` | `() => string` | Returns the on-disk file path of the vault. |
| `accessProfile` | `() => VaultAccessProfile` | Returns the access profile (`ReadWrite` or `ReadOnly`). |
| `structureBinding` | `() => VaultStructureBinding` | Returns the structure binding schema (`Legacy` or `Bound`). |
| `rollbackSuspected` | `() => boolean` | Indicates whether a rollback was detected upon opening. |
| `rotationDue` | `() => VaultRotationDue[]` | Returns list of secrets whose `rotate_policy` tag is overdue. |

---

## `KvVault`

A synchronous local key–value store powered by `fjall`. Used as an opaque blob backend to persist serialized vault files or arbitrary strings locally.

| Method | Signature | Description |
| --- | --- | --- |
| `new KvVault(path)` | `(path: string) => KvVault` | Opens or creates a `fjall` database at `path`. |
| `put` | `(key: string, value: string) => void` | Stores a string value under `key`. |
| `get` | `(key: string) => string \| null` | Retrieves the stored value for `key`, or `null` if absent. |
| `delete` | `(key: string) => void` | Removes `key` from the database. |
| `list` | `(prefix: string) => string[]` | Lists all sorted keys matching `prefix` (`""` lists all). |

---

## `KeyManager`

File-based key manager for handling symmetric keys in repository settings. Available when compiled with crypto support.

| Method | Signature | Description |
| --- | --- | --- |
| `new KeyManager(gitDir)` | `(gitDir: string) => KeyManager` | Initializes a manager for `gitDir`. |
| `generateKey` | `() => Buffer` | Generates a fresh random key, saves it, and returns bytes. |
| `loadKey` | `() => Buffer` | Loads the default key from disk and returns raw bytes. |
| `saveKey` | `(key: Buffer) => void` | Saves raw key bytes as the default key file. |
| `importKey` | `(inputPath: string) => void` | Imports a key from a file path as the default key. |
| `defaultKeyPath` | `() => string` | Returns the default key file path. |

---

## Enums and Objects

### `VaultStructureBinding` (string enum)
- `"Legacy"`: v1/v2 format without per-entry AAD / whole-file MAC.
- `"Bound"`: v3 format with structural integrity binding.

### `VaultAccessProfile` (string enum)
- `"ReadWrite"`: Allows reading and mutating secrets.
- `"ReadOnly"`: Mutations throw errors.

### `VaultHistoryEntry` (object)
```ts
interface VaultHistoryEntry {
  version: number;
  timestamp: string; // RFC3339 formatted date-time
}
```

### `VaultRotationDue` (object)
```ts
interface VaultRotationDue {
  key: string;
  overdueDays: number;
  policy: string; // e.g. "90d"
}
```

---

## Example Usage

```ts
import { Vault, KvVault } from "./main.ts";

// 1. Working with encrypted Vault files
const password = Buffer.from("super-secret-passphrase");
const vault = new Vault("/path/to/app.vault", password);

vault.set("DATABASE_URL", "postgres://user:pass@localhost:5432/db", {
  rotate_policy: "30d"
});
vault.save();

console.log(vault.get("DATABASE_URL"));
console.log(vault.rotationDue());

// 2. Using KvVault for local key-value storage
const kv = new KvVault("/path/to/kv-store");
kv.put("app:config", JSON.stringify({ theme: "dark" }));
const config = kv.get("app:config");
console.log(kv.list("app:"));
```
