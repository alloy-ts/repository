# Repository

Git-based Source Code Management (SCM) system `Repository` Node-API (N-API)
bindings for the Rust [`gitoxide`](https://github.com/GitoxideLabs/gitoxide)
(`gix`) crate.

`@alloy-ts/repository` exposes `gix::Repository` capabilities to
JavaScript/TypeScript, providing safe, high-speed Git operations with modern
async support and full type definitions. It also includes encrypted vault management
(`Vault`), local `fjall`-backed key-value persistence (`KvVault`), `KeyManager`,
and `IdentityManager` bindings.

## Features

- **Git Operations**: Fast repository discovery, object inspection, references, commits, diffs, blame, submodules, and worktrees via `gitoxide`.
- **Secret Vault**: Native `Vault` file format support, version history management, structural MAC verification, and key rotation.
- **Key-Value Store**: Local synchronous `KvVault` store backed by `fjall` for opaque blob persistence.

## Documentation

Detailed API documentation can be found in `docs/api/`:
- [`Vault` & `KvVault` API](docs/api/vault.md)
- [`Repository` API](docs/api/repository.md)

## Getting Started

1. Clone the repository
2. Install dependencies
3. Build the native addon

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
