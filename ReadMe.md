# Repository

Git-based Source Code Management (SCM) system `Repository` Node-API (N-API)
bindings for the Rust [`gitoxide`](https://github.com/GitoxideLabs/gitoxide)
(`gix`) crate.

`@alloy-ts/repository` exposes `Repository` capabilities to
JavaScript/TypeScript, providing safe, high-speed Git operations with modern
async support and full type definitions.

## Features

- **Git Operations**: Repositories, commits, trees, blobs, diffs, blame, status,
  worktrees, branches, tags, and remotes. See
  [Git API Documentation](docs/api/repo/index.md).
- **Cryptography & Vault**: Symmetric ChaCha20-Poly1305 / AES encryption,
  ref-backed key vaults, git-crypt worktree filter drivers, and
  git-remote-gcrypt entire repository mode (packfile & manifest encryption).
- **OpenPGP & GPG**: OpenPGP encryption, decryption, signing, and signature
  verification via `rPGP`, along with GPG keyring lookup utilities. See
  [Cryptography API Documentation](docs/api/crypto/index.md).
- **SQL Schema Compiler**: Turns a JavaScript/TypeScript schema definition into
  validated Rust models, Diesel tables, NAPI-RS bindings, SQL DDL, runtime
  validators, and migrations for SQLite and PostgreSQL. See
  [SQL Schema Compiler Documentation](docs/api/sql/index.md).
- **Task & Process Execution**: Background worker threads, child process
  spawning, and scheduling. See
  [Task API Documentation](docs/api/task/index.md).
- **Engine Runtime**: Bytecode compilation and FuseVM VM execution. See
  [Engine API Documentation](docs/api/engine/index.md).

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

## Workspace layout

The root coordinates npm and Cargo workspaces. The existing Rust/N-API library,
TypeScript entry points, and tests live in `workspaces/repository/src/`; its
Cargo manifest and native build script live in `workspaces/repository/`.
The private npm member is named `@alloy-ts/repository-source`. npm discovers
packages under `packages/*`, `crates/*`, `workspaces/*`, and `modules/*`. Add
new Rust members to the root Cargo workspace's `members` list.

Run `npm install`, `cargo build`, `npm run build`, and `npm test` from the
repository root. Generated bindings and package artifacts remain in `dist/`,
and all Rust members share the root `Cargo.lock` and `target/` directory.
