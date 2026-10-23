# Repository

Git-based Source Code Management (SCM) system `Repository` Node-API (N-API)
bindings for the Rust [`gitoxide`](https://github.com/GitoxideLabs/gitoxide)
(`gix`) crate.

`@alloy-ts/repository` exposes `gix::Repository` capabilities to
JavaScript/TypeScript, providing safe, high-speed Git operations with modern
async support and full type definitions.

## Getting Started

1. Clone the repository
2. Install dependencies
3. Build native bindings

```bash
git clone https://repo.chen.software/yumin-chen/repository.git
cd repository
npm install
npm run build
```

## Running the application

```bash
npm run start
```

## Testing the application

```bash
npm run test
```

## Features & Modules

### Git Object Replacement (`refs/replace/*`)

Replace git objects directly or maintain revision history trees at `refs/replace/`:

- `replace(repo, oldOid, newContent, options)`: Replaces a git object. Checks type matching unless `-f` (force) is supplied. Respects `core.useReplaceRefs` and `GIT_NO_REPLACE_OBJECTS`.
- `tryReplace(repo, oldOid, newContent, options)`: Fallback attempt to replace git objects.
- `useReplaceRefs(repo)`: Reports whether replace refs are enabled.
- `getReplaceManifest(repo, key)`: Retrieves `ReplaceManifest` JSON containing revision history (`versions`, `head`, `size`).
- `getReplaceNode(repo, key, revisionId)` / `getRevision(...)`: Retrieves a specific revision node from a replace manifest.
- `getReplaceRef(repo, key)`: Returns the underlying target object ID for a replace reference.
- `derefReplace(repo, oid)`: Dereferences an object ID to its latest replacement ID when replace refs are enabled.

### Encrypted Vault & Secret Management

- `Vault`: Manage encrypted vault homes, secret entries, canonical fields (`setField`, `removeField`, `listFields`), and authenticated snapshots.
- `CredentialCipher`: Versioned AES-256-GCM keyring for scope-bound secret encryption (`encrypt`, `decrypt`).
- `CredentialSecrets`: High-level container for plaintext credential secrets before encryption.
- `KvEngine`: KV v2 secret store engine interface for versioned key-value operations (`read`, `write`, `delete`, `list`, `metadata`).
