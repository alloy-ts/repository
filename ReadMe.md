# Repository

Git-based Source Code Management (SCM) system `Repository` Node-API (N-API)
bindings for the Rust [`gitoxide`](https://github.com/GitoxideLabs/gitoxide)
(`gix`) crate.

`@alloy-ts/repository` exposes `gix::Repository` capabilities to
JavaScript/TypeScript, providing safe, high-speed Git operations with modern
async support, encrypted vault storage, and full type definitions.

## Features

- **Repository & Git Operations**: Fast repository discovery, object access, references, diffs, index manipulation, and commit graph processing powered by `gitoxide`.
- **Encrypted Vault Storage (`Vault`)**: Secure file-backed secret storage using Argon2id key derivation and XChaCha20Poly1305 / AES AEAD ciphers with version history and rotation policy tracking.
- **Embedded Key-Value Store (`KvVault`)**: Fast, synchronous local key-value database backed by `fjall` for storing opaque string or serialized vault payloads.
- **Key & Crypto Management (`KeyManager` / Web Crypto)**: Repository key creation, import/export, and Web Crypto API compatible cryptographic bindings.

## API Documentation

Detailed documentation for NAPI bindings is available in [`docs/api/`](docs/api/):

- [`Vault` & `KvVault` API](docs/api/vault.md) - Encrypted secret storage, version history, and `fjall`-backed key-value store.
- [`Index` API](docs/api/index.md) - Git index and staging area manipulation.
- [`Object` API](docs/api/object.md) - Git objects (blobs, commits, trees, tags).
- [`ObjectStore` API](docs/api/object-store.md) - Reading and writing loose and packed objects.
- [`Revision` API](docs/api/revision.md) - Revision parsing and walking.
- [`Diff` API](docs/api/diff.md) - Tree and blob diffing.
- [`Path` API](docs/api/path.md) - Pathspec parsing and path utilities.
- [`Refspec` API](docs/api/refspec.md) - Fetch and push refspec parsing.
- [`Blame` API](docs/api/blame.md) - Git blame computation.

## Getting Started

1. Clone the repository
2. Install dependencies
3. Run the build

```bash
git clone https://repo.chen.software/yumin-chen/repository.git
cd repository
npm install
npm run build
```

## Running tests

```bash
npm run test
```
