# Vault & KvVault API

`src/vault.rs` provides synchronous, file-backed encrypted secrets storage ([`Vault`](#vault)) and an embedded key–value database ([`KvVault`](#kvvault)) for storing opaque payload blobs (such as encrypted vault files).

All crypto operations use Argon2id KDF key derivation, XChaCha20-Poly1305 AEAD authenticated encryption, and HKDF-SHA256 key scheduling. Secret values are stored in zeroizing memory buffers.

---

## `Vault`

JS-facing handle to an open vault. All operations lock internally via a `Mutex`.

```ts
import { Vault, VaultAccessProfile, VaultStructureBinding } from "@alloy-ts/repository";
```

### Constructors & Opening

| Method | Signature | Description |
| --- | --- | --- |
| `new Vault(path, password)` | `(path: string, password: Buffer) => Vault` | Creates a new encrypted vault at `path`. |
| `open` | `(path: string, password: Buffer, readOnly?: boolean) => Vault` | Opens an existing vault file. |
| `openWithKey` | `(path: string, key: Buffer, readOnly?: boolean) => Vault` | Opens a vault using a raw 32-byte key (DEK). |

### Secret Operations

| Method | Signature | Description |
| --- | --- | --- |
| `set` | `(key: string, value: string, tags?: Record<string, string>) => void` | Upserts a secret in memory. |
| `get` | `(key: string) => string` | Decrypts and returns a secret's plaintext value. |
| `delete` | `(key: string) => void` | Removes a secret from memory. |
| `list` | `() => string[]` | Returns all secret key names, sorted. |
| `renameKey` | `(oldKey: string, newKey: string, overwrite?: boolean) => void` | Renames a secret key. |
| `rotate` | `(newPassword: Buffer) => void` | Re-encrypts the vault under a new master password. |
| `exportAll` | `() => Record<string, string>` | Decrypts and exports all secret key-value pairs. |
| `save` | `() => void` | Flushes in-memory mutations to disk atomically. |

### Versioning & History

| Method | Signature | Description |
| --- | --- | --- |
| `getVersion` | `(key: string, version: number) => string` | Decrypts a historical version (`0` = current). |
| `history` | `(key: string) => VaultHistoryEntry[]` | Lists historical version metadata for a key. |
| `revertToVersion` | `(key: string, version: number) => void` | Reverts a secret to a previous version. |
| `pruneHistory` | `(key: string, keepN: number) => void` | Retains at most `keepN` historical entries. |

### Metadata & Inspection

| Method | Signature | Description |
| --- | --- | --- |
| `secretCount` | `() => number` | Returns the total number of stored secrets. |
| `path` | `() => string` | Returns the filesystem path of the vault file. |
| `accessProfile` | `() => VaultAccessProfile` | Returns `ReadWrite` or `ReadOnly`. |
| `structureBinding` | `() => VaultStructureBinding` | Returns `Legacy` (v1/v2) or `Bound` (v3). |
| `rollbackSuspected` | `() => boolean` | Returns `true` if the version counter went backwards. |
| `rotationDue` | `() => VaultRotationDue[]` | Returns secrets overdue for rotation according to `rotate_policy` tags. |

### Static Methods

| Method | Signature | Description |
| --- | --- | --- |
| `isTeamVault` | `(path: string) => boolean` | Checks whether `path` is an age-wrapped team vault. |
| `parseRotationDays` | `(policy: string) => bigint \| null` | Parses duration strings like `"90d"` into days. |

---

## `KvVault`

Embedded key-value database backed by `fjall`. Stores opaque string blobs (e.g., serialized `VaultFile` JSON payloads).

```ts
import { KvVault } from "@alloy-ts/repository";
```

| Method | Signature | Description |
| --- | --- | --- |
| `new KvVault(path)` | `(path: string) => KvVault` | Opens/creates a KV database at `path`. |
| `put` | `(key: string, value: string) => void` | Stores a string value under `key`. |
| `get` | `(key: string) => string \| null` | Retrieves the stored value for `key`, or `null`. |
| `delete` | `(key: string) => void` | Deletes `key` from the store. |
| `list` | `(prefix: string) => string[]` | Lists all keys matching `prefix`, sorted. |

---

## Enums & Object Types

### `VaultStructureBinding`
- `"Legacy"`: Unbound v1/v2 format (no AAD, no whole-file MAC).
- `"Bound"`: v3 format (per-entry AAD + structural MAC).

### `VaultAccessProfile`
- `"ReadWrite"`: Allows mutations and `save()`.
- `"ReadOnly"`: Rejects all state modifications.

### `VaultHistoryEntry`
- `version`: Version index (`0` = current).
- `timestamp`: RFC3339 timestamp when written.

### `VaultRotationDue`
- `key`: Secret key name.
- `overdueDays`: Days past rotation due date.
- `policy`: Policy tag value (e.g. `"90d"`).
