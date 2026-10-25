# Repository

Git-based Source Code Management (SCM) system `Repository` Node-API (N-API)
bindings for the Rust [`gitoxide`](https://github.com/GitoxideLabs/gitoxide)
(`gix`) crate and GPG cryptographic operations via [`gpgme`](https://crates.io/crates/gpgme).

`@alloy-ts/repository` exposes `gix::Repository` capabilities and full GPG encryption, decryption, signing, and verification bindings to JavaScript/TypeScript with modern async support and full type definitions.

## Features

- **Git Repository Operations**: Open, discover, clone, status, commit log, cat-file, attributes, and revision parsing powered by `gitoxide`.
- **GPG Integration**: High-performance GPG operations using `gpgme` including encryption, decryption, signing, verification, and key inspection.
- **Git-Crypt & Crypto Utilities**: Filter configuration, path inspection, and symmetric key operations.

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

## Running the application

```bash
npm run start
```

## Testing the application

```bash
npm run test
```

## Documentation

- [GPG API Documentation](docs/api/gpg.md)
- [Crypto & Git-Crypt API Documentation](docs/api/crypto.md)
