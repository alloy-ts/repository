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

## Cryptography & GPG Support

`Repository` provides cryptographic primitives and GPG integration backed by `rPGP` (`pgp` crate) and native Rust crypto:

- **GPG Subsystem (`src/gpg.rs`)**: Perform OpenPGP encryption, decryption, signing, and signature verification with `Context`, `Key`, `Data`, `VerificationResult`, and `Protocol`. Includes CLI utility functions for key lookup, listing secret keys, and file encryption/decryption.
- **Crypto Subsystem (`src/crypto.rs`)**: Symmetric encryption (ChaCha20-Poly1305), SSH identity management, XMSS^MT stateful signatures, and git-crypt filter configuration.
