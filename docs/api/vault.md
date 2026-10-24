# Vault API

The Vault module in `src/vault.rs` provides secure, encrypted key-value storage with structure-bound AEAD encryption, versioning, rotation policy checks, and advisory process locking. It also includes `KvVault` for embedded local key-value store persistence via `fjall`, `KeyManager` for git-crypt symmetric keys, and `IdentityManager` for SSH recipient key sharing.

---

## `Vault` Class

A handle to an open or created vault. Handles internally serialize access with a mutex so instance methods can be called safely.

### Constructors and Static Methods

| Method | Signature | Description |
| --- | --- | --- |
| `new Vault` | `(path: string, password: Buffer) => Vault` | Create a new vault on disk at `path` encrypted with `password`. |
| `Vault.open` | `(path: string, password: Buffer, readOnly?: boolean) => Vault` | Open an existing vault. Pass `readOnly: true` to prevent any writes. |
| `Vault.openWithKey` | `(path: string, key: Buffer, readOnly?: boolean) => Vault` | Open an existing vault using a raw 32-byte key. |
| `Vault.createWithAccessProfile` | `(path: string, password: Buffer, accessProfile: VaultAccessProfile) => Vault` | Create a new vault with a specific access profile (`"ReadWrite"` or `"ReadOnly"`). |
| `Vault.isTeamVault` | `(path: string) => boolean` | Check whether the vault on disk has age recipients and a wrapped DEK. |
| `Vault.parseRotationDays` | `(policy: string) => number \| null` | Parse a duration policy string like `"90d"` into number of days. |
| `Vault.liveLockHolder` | `(path: string) => number \| null` | Return the PID of a live process holding the vault lock, if any. |
| `Vault.lockState` | `(path: string) => VaultLockStateInfo \| null` | Return detailed lock state for the vault at `path` if locked. |
| `Vault.validateSecretKey` | `(key: string) => void` | Validate a secret key name according to vault rules; throws on error. |
| `Vault.isSupportedSchema` | `(schema: string) => boolean` | Check whether `schema` is supported. |
| `Vault.schemaIsBound` | `(schema: string) => boolean` | Check whether `schema` implies structure binding (schema v3). |

### Instance Methods

| Method | Signature | Description |
| --- | --- | --- |
| `set` | `(key: string, value: string, tags?: Record<string, string>) => void` | Insert or update a secret value with optional tags. |
| `get` | `(key: string) => string` | Decrypt and return a secret's live value. |
| `delete` | `(key: string) => void` | Remove a secret entry. |
| `list` | `() => string[]` | List all secret keys in sorted order. |
| `renameKey` | `(oldKey: string, newKey: string, overwrite?: boolean) => void` | Atomically rename a secret key. |
| `rotate` | `(newPassword: Buffer) => void` | Re-encrypt all secrets under a new master password. |
| `secretCount` | `() => number` | Return the total number of secret entries stored in the vault. |
| `save` | `() => void` | Persist in-memory mutations to disk. |
| `getVersion` | `(key: string, version: number) => string` | Decrypt a specific historical version of a secret (0 = current). |
| `history` | `(key: string) => VaultHistoryEntry[]` | Return historical version metadata for a secret. |
| `revertToVersion` | `(key: string, version: number) => void` | Revert a secret's current value to a historical version. |
| `pruneHistory` | `(key: string, keepN: number) => void` | Prune secret version history keeping at most `keepN` previous versions. |
| `exportAll` | `() => Record<string, string>` | Decrypt and return all secret key-value pairs as a plain object. |
| `path` | `() => string` | Return the on-disk file path of the vault. |
| `accessProfile` | `() => VaultAccessProfile` | Return the access profile (`"ReadWrite"` or `"ReadOnly"`). |
| `structureBinding` | `() => VaultStructureBinding` | Return the structure binding mode (`"Legacy"` or `"Bound"`). |
| `rollbackSuspected` | `() => boolean` | Check if a rollback was detected when the vault was opened. |
| `rotationDue` | `() => VaultRotationDue[]` | Return secrets whose `rotate_policy` tag is overdue for rotation. |

---

## `KvVault` Class

A synchronous embedded key-value store backed by `fjall` for storing opaque string/blob payloads.

| Method | Signature | Description |
| --- | --- | --- |
| `new KvVault` | `(path: string) => KvVault` | Open or create a KV store database partition at `path`. |
| `put` | `(key: string, value: string) => void` | Store a value under a key. |
| `get` | `(key: string) => string \| null` | Fetch the string value stored under a key, or `null` if missing. |
| `delete` | `(key: string) => void` | Remove a key from the store. |
| `list` | `(prefix: string) => string[]` | List keys matching a prefix in sorted order. |

---

## `KeyManager` Class

Manages repository symmetric encryption keys located under `.git/crypt/keys`.

| Method | Signature | Description |
| --- | --- | --- |
| `new KeyManager` | `(gitDir: string) => KeyManager` | Create a key manager rooted at the specified `.git` directory. |
| `generateKey` | `() => Buffer` | Generate and save a new random symmetric key; returns the raw key bytes. |
| `loadKey` | `() => Buffer` | Load the default symmetric key from disk. |
| `saveKey` | `(key: Buffer) => void` | Save raw key bytes as the default symmetric key. |
| `importKey` | `(inputPath: string) => void` | Import a key file as the default symmetric key. |
| `defaultKeyPath` | `() => string` | Return the default symmetric key file path. |
| `gitCryptDir` | `() => string` | Return the path to the `<gitDir>/crypt` directory. |
| `initDirs` | `() => void` | Initialize the `crypt` and `crypt/keys` directories. |
| `isInitialized` | `() => boolean` | Check if the repository has been initialized for `crypt`. |
| `exportKey` | `(outputPath: string) => void` | Export the default key to `outputPath`. |

---

## `IdentityManager` Class

Static utilities for encrypting/decrypting repository symmetric keys using SSH identities.

| Method | Signature | Description |
| --- | --- | --- |
| `IdentityManager.encryptKeyForSshRecipient` | `(key: Buffer, recipient: string) => Buffer` | Encrypt a symmetric key for an ed25519 SSH recipient. |
| `IdentityManager.decryptKeyWithSshIdentity` | `(encrypted: Buffer, identityContent: string, passphrase?: string) => Buffer` | Decrypt an envelope using an SSH private key identity. |

---

## Types and Enums

```ts
export type VaultStructureBinding = "Legacy" | "Bound";
export type VaultAccessProfile = "ReadWrite" | "ReadOnly";

export interface VaultHistoryEntry {
  version: number;
  timestamp: string;
}

export interface VaultLockStateInfo {
  state: string; // e.g. "HeldBy" or "Unparseable"
  pid?: number;
}

export interface VaultRotationDue {
  key: string;
  overdueDays: number;
  policy: string;
}
```
