# Vault & KvVault API Documentation

This module provides native Node.js (napi-rs) bindings for managing encrypted personal/team secret vaults (`VaultHandle`) and local embedded key-value storage (`KvVaultHandle`) backed by `fjall`.

## `VaultHandle`

`VaultHandle` represents an unlocked, in-memory vault file.

### Static Methods

- `VaultHandle.create(path: string, password: Buffer | string): VaultHandle`
  Creates a new vault at the specified path using Argon2id KDF and default cipher configuration.

- `VaultHandle.open(path: string, password: Buffer | string): VaultHandle`
  Opens and authenticates an existing vault with read-write access profile.

- `VaultHandle.openReadOnly(path: string, password: Buffer | string): VaultHandle`
  Opens and authenticates an existing vault in read-only mode.

- `VaultHandle.openWithKey(path: string, key: Buffer): VaultHandle`
  Opens a vault using a pre-derived 32-byte master key.

- `VaultHandle.isTeamVault(path: string): boolean`
  Checks whether a vault file carries team recipient metadata without requiring authentication.

### Instance Methods

- `save(): void`
  Flushes buffered secret changes back to disk atomically with structural integrity tags.

- `set(key: string, value: string, tags?: Record<string, string>): void`
  Sets or updates a secret key-value entry.

- `get(key: string): string`
  Decrypts and returns a secret value.

- `delete(key: string): void`
  Deletes a secret entry by key name.

- `list(): string[]`
  Returns all secret key names in sorted order.

- `exportAll(): Record<string, string>`
  Decrypts and returns all secrets as a key-value mapping.

- `renameKey(oldKey: string, newKey: string, overwrite?: boolean): void`
  Renames a secret key entry, re-binding its associated data.

- `getVersion(key: string, version: number): string`
  Retrieves a specific historical version of a secret (`0` for current value).

- `history(key: string): VaultHistoryEntry[]`
  Returns version metadata history entries for a key, newest first.

- `revertToVersion(key: string, version: number): void`
  Reverts a secret to a previous version number.

- `pruneHistory(key: string, keepN: number): void`
  Prunes version history for a secret, retaining at most `keepN` entries.

- `rotate(newPassword: Buffer | string): void`
  Re-encrypts all secrets and challenge entries under a new master password.

- `rotationDue(): VaultRotationDue[]`
  Returns list of secrets overdue for rotation based on `rotate_policy` tags.

### Getters

- `path: string`: Vault file path.
- `secretCount: number`: Total number of active secrets.
- `accessProfile: VaultAccessProfile`: `VaultAccessProfile.ReadWrite` or `VaultAccessProfile.ReadOnly`.
- `structureBinding: VaultStructureBinding`: `VaultStructureBinding.Legacy` or `VaultStructureBinding.Bound`.
- `rollbackSuspected: boolean`: Indicates if the vault write counter went backwards relative to high-water mark records.

---

## `KvVaultHandle`

`KvVaultHandle` provides synchronous local key-value storage backed by an embedded `fjall` partition.

### Static Methods

- `KvVaultHandle.open(path: string): KvVaultHandle`
  Opens or creates an embedded key-value store at the target directory path.

### Instance Methods

- `put(key: string, value: string): void`
  Puts a key-value pair into the partition.

- `get(key: string): string | null`
  Gets a stored value by key name, or `null` if not found.

- `delete(key: string): void`
  Deletes a key-value pair.

- `list(prefix: string): string[]`
  Lists all stored keys matching a given prefix.

---

## Enums & Objects

### `VaultStructureBinding`
- `Legacy`: Schema v1 / v2 without structural MAC binding.
- `Bound`: Schema v3 with per-entry AAD and whole-file MAC.

### `VaultAccessProfile`
- `ReadWrite`: Full read and write capabilities.
- `ReadOnly`: Mutations and `save()` operations are blocked.

### `VaultHistoryEntry`
- `version: number`
- `timestamp: string` (RFC3339)

### `VaultRotationDue`
- `key: string`
- `overdueDays: number`
- `policy: string`
