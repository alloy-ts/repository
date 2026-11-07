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

The root coordinates npm and Cargo workspaces. Domain implementations and tests
live in `sources/{repo,crypto,object_store,sql,task,engine}/src/`. Each domain has
its own private npm `package.json` and Rust `Cargo.toml`, with shared package
metadata and dependencies defined in the root Cargo manifest.

`sources/repository` is the integration crate and combined TypeScript entry point.
It compiles the domain implementation modules into one N-API library, preserving
shared native types and existing cross-domain references. Domain Rust crates
provide façades over that library; their npm entry points re-export the combined
bindings. These are workspace boundaries, not independent native addons. This
avoids Cargo dependency cycles between the repository, object store, and task APIs.

npm discovers packages under `packages/*`, `crates/*`, `workspaces/*`, `modules/*`,
and `sources/*`. Cargo discovers members under `sources/*`.

Run `npm install`, `cargo build`, `npm run build`, and `npm test` from the root.
Use `cargo check --workspace` to validate every domain façade. Generated bindings
and package artifacts remain in `dist/`; Rust members share `Cargo.lock` and
`target/` at the root. Tests are discovered under `sources/*/src/`.
