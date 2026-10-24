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
3. Run the application

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

## Secret Vault & Key-Value Storage

`@alloy-ts/repository` includes native secret vault and key-value storage capabilities via `VaultHandle` and `KvVaultHandle`.

### Vault Features
- **AEAD & Structural Integrity**: Secrets encrypted with XChaCha20Poly1305 / AES-256-GCM and bound with canonical structure MACs (schema v3).
- **Version History & Revert**: Secret history entries with versioning, revert, and pruning support.
- **Key-Value Partition (`KvVaultHandle`)**: Embedded synchronous key-value storage powered by `fjall`.

See [`docs/api/vault.md`](docs/api/vault.md) for full API documentation.
