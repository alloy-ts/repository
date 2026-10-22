# Crypto API (`@alloy-ts/crypto`)

The Crypto module provides Web Crypto API bindings and `blsful` signature operations, replacing Node's OpenSSL dependency with pure Rust implementations built on top of `blsful` and `ring`.

## `node:crypto` vs `@alloy-ts/crypto` Implementation

| Web Crypto / `node:crypto` Feature | Node.js Core (`node:crypto`) | `@alloy-ts/crypto` | Backing Implementation |
| :--- | :--- | :--- | :--- |
| `CryptoKey` object binding | Object class | `#[napi(object)] CryptoKey` | NAPI-RS Web Crypto bindings |
| `CryptoKey.Format` / `CryptoKeyFormat` | Static / enum | `CryptoKeyFormat` enum | NAPI-RS enum (`Raw`, `Pkcs8`, `Spki`, `Jwk`, `RawSecret`, `RawPublic`, `RawSeed`) |
| `Signature` helper constructor | OpenSSL / Web Crypto | `new Signature(keyFormat)` | NAPI-RS constructor binding |
| `BLS12-381` Signatures / Threshold Shares | Not supported in standard Node.js | Supported (`bls12_381-g1-msig`, `bls12_381-g2-msig`) | Pure Rust `blsful` crate |
| `Multisig` & `MultisigBuilder` | Not supported in Node.js | Supported | `multi-sig` / `blsful` |
| `SignatureBytes` & `SignatureScheme` | Not supported in Node.js | Supported | NAPI-RS bindings |
| AES-GCM / ChaCha20-Poly1305 | OpenSSL | Supported | `chacha20poly1305` / `ring` Rust crates |
| Ed25519 / X25519 | OpenSSL | Supported | `ed25519-dalek` / `curve25519-dalek` Rust crates |

## Usage Examples

### CryptoKey Object

```ts
import { CryptoKey, CryptoKeyFormat, Signature } from "@alloy-ts/crypto";

const sig = new Signature(CryptoKeyFormat.Raw);
console.log(sig.keyFormat); // CryptoKeyFormat.Raw
```

### BLS Threshold Signatures with `MultisigBuilder`

```ts
import { MultisigBuilder } from "@alloy-ts/crypto";

const ms = new MultisigBuilder("eddsa-msig")
  .withSignature(signatureBuffer)
  .tryBuild();
```
