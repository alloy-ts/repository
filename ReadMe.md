# Repository

Git-based Source Code Management (SCM) system `Repository` Node-API (N-API)
bindings for the Rust [`gitoxide`](https://github.com/GitoxideLabs/gitoxide)
(`gix`) crate.

`@alloy-ts/repository` exposes `gix::Repository` capabilities to
JavaScript/TypeScript, providing safe, high-speed Git operations with modern
async support and full type definitions.

## Features

- **Git Operations**: Repositories, commits, trees, blobs, diffs, blame, status, worktrees, branches, tags, and remotes.
- **Cryptography & Vault**: Symmetric ChaCha20-Poly1305 / AES encryption, ref-backed key vaults, git-crypt worktree filter drivers, and git-remote-gcrypt entire repository mode (packfile & manifest encryption).
- **OpenPGP & GPG**: OpenPGP encryption, decryption, signing, and signature verification via `rPGP`, along with GPG keyring lookup utilities. See [Cryptography API Documentation](docs/api/crypto.md).

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
