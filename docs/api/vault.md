# Vault API Documentation

The `vault` module provides native Node.js bindings for encrypted secret vault management, on-disk key storage (`KeyManager`), SSH recipient key operations (`IdentityManager`), and synchronous local key-value persistence (`KvVault`).

---

## Classes

### `Vault`

An open, authenticated vault file representation.

#### Static Methods

- `Vault.open(path: string): Vault`
  Opens and parses an existing vault file from disk.
- `Vault.openWithKey(path: string, keyBuffer: Buffer): Vault`
  Opens a vault file using a pre-derived 32-byte key buffer.
- `Vault.status(path: string): VaultStatus`
  Inspects a vault file on disk without requiring master key authentication.
- `Vault.isTeamVault(path: string): boolean`
  Checks if the specified vault file contains age recipient public keys and a wrapped DEK.

#### Instance Methods

- `path(): string`
  Returns the file path of the vault.
- `schema(): string`
  Returns the declared schema version of the vault file (`tsafe/vault/v1`, `v2`, or `v3`).
- `accessProfile(): VaultAccessProfile`
  Returns the access profile (`ReadWrite` or `ReadOnly`).
- `structureBinding(): VaultStructureBinding`
  Returns whether secret ciphertexts are bound to structure/key names (`Legacy` or `Bound`).
- `rollbackSuspected(): boolean`
  Returns whether a version counter rollback was detected.
- `secretCount(): number`
  Returns the number of secrets stored in the vault.
- `list(): string[]`
  Returns a sorted array of secret key names.
- `isTeamVault(): boolean`
  Returns `true` if the vault contains age recipients and a wrapped DEK.
- `getSecretEntry(key: string): SecretEntry | null`
  Returns secret metadata and ciphertext entry for the given key name.
- `getVersion(key: string, version: number): string`
  Returns the ciphertext of a specific version of a secret (`0` is live, `1` is previous, etc.).
- `history(key: string): VaultHistoryEntry[]`
  Returns version number and timestamp history entries for a secret key.
- `revertToVersion(key: string, version: number): void`
  Reverts a secret key to a previous version and persists the updated vault file.
- `pruneHistory(key: string, keepN: number): void`
  Prunes historical secret entries to keep at most `keepN` previous versions.
- `exportAll(): Record<string, string>`
  Returns a map of secret key names to their ciphertexts.
- `save(): void`
  Persists the vault file to disk.

---

### `KvVault`

A synchronous local key-value store backed by `fjall`. Stores data as opaque key-value blobs on disk.

#### Static Methods

- `KvVault.open(path: string): KvVault`
  Opens or creates a `fjall` keyspace and partition at the specified directory path.

#### Instance Methods

- `put(key: string, value: string): void`
  Stores a key-value pair.
- `get(key: string): string | null`
  Retrieves a stored value by key name, or `null` if not found.
- `delete(key: string): void`
  Deletes a stored key-value pair.
- `list(prefix: string): string[]`
  Returns all stored key names matching the given prefix.

---

### `KeyManager`

Manages on-disk key files under `<git-dir>/crypt`.

#### Constructor

- `new KeyManager(gitDir: string)`

#### Instance Methods

- `gitCryptDir(): string`
- `defaultKeyPath(): string`
- `initDirs(): void`
- `isInitialized(): boolean`

---

### `IdentityManager`

Helper functions for SSH recipient key encryption and decryption envelopes.

---

## Data Types & Enums

### `VaultStructureBinding`
`"Legacy"` | `"Bound"`

### `VaultAccessProfile`
`"ReadWrite"` | `"ReadOnly"`

### `VaultStatus`
```ts
interface VaultStatus {
  root: string;
  exists: boolean;
  formatVersion?: number;
  secretCount: number;
  isTeamVault: boolean;
}
```

### `VaultHistoryEntry`
```ts
interface VaultHistoryEntry {
  version: number;
  timestamp: string;
}
```

### `VaultRotationDue`
```ts
interface VaultRotationDue {
  key: string;
  overdueDays: number;
  policy: string;
}
```
