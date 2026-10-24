# Repository

Git-based Source Code Management (SCM) system `Repository` Node-API (N-API)
bindings for the Rust [`gitoxide`](https://github.com/GitoxideLabs/gitoxide)
(`gix`) crate, encrypted file-backed `Vault`, and embedded `KvVault` storage.

`@alloy-ts/repository` exposes `gix::Repository` capabilities to
JavaScript/TypeScript, providing safe, high-speed Git operations with modern
async support, encrypted secret vault management (`Vault`), local embedded key-value storage (`KvVault`), and full type definitions.

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
