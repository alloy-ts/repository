# Crypto & Web Crypto API

Transparent file encryption (`crypt`) layered on top of the repository via ChaCha20-Poly1305, as well as Web Crypto `CryptoKey` NAPI bindings for key structures and feature detection.

## `CryptoKey` & `CryptoKeyPair` Bindings

Exposes `CryptoKey` and `CryptoKeyPair` object structures matching Web Crypto type specifications.

### Key Formats (`KeyFormat`)

| Format | String Value | Description |
| --- | --- | --- |
| `KeyFormat.Raw` | `"raw"` | Raw key bytes (symmetric keys / public keys). |
| `KeyFormat.RawSecret` | `"raw-secret"` | Raw secret key bytes. |
| `KeyFormat.RawPublic` | `"raw-public"` | Raw public key bytes. |
| `KeyFormat.RawSeed` | `"raw-seed"` | Raw seed bytes. |
| `KeyFormat.Pkcs8` | `"pkcs8"` | PKCS#8 private key structure in PEM or DER encoding. |
| `KeyFormat.Spki` | `"spki"` | SubjectPublicKeyInfo public key structure in PEM or DER encoding. |
| `KeyFormat.Jwk` | `"jwk"` | JSON Web Key object representation. |

### API Comparison: Upstream `node:crypto` vs Our Implementation

| API / Feature | Upstream `node:crypto` Web Crypto API | Our Implementation | Supported Algorithms / Notes |
| --- | --- | --- | --- |
| `CryptoKey.supports()` | Static feature detection method | `cryptoKeySupports(operation, algorithm)` | Performs runtime feature detection for Web Crypto operations (`encrypt`, `decrypt`, `sign`, `verify`, `digest`, `deriveKey`, `deriveBits`, `importKey`, `exportKey`, `wrapKey`, `unwrapKey`, `encapsulateBits`, `decapsulateBits`, `encapsulateKey`, `decapsulateKey`). |
| `CryptoKey` | Object representation | `CryptoKey` (`#[napi(object)]`) | Represents a key with `algorithm`, `extractable`, `type`, and `usages` properties. |
| `CryptoKeyPair` | Object `{ publicKey, privateKey }` | `CryptoKeyPair` (`#[napi(object)]`) | Holds `publicKey` and `privateKey` instances. |
| Repository File Encryption | `subtle` Web Crypto / cipher streams | `Crypto.encrypt()` / `Crypto.decrypt()` | Transparent ChaCha20-Poly1305 symmetric encryption for git-crypt files with magic `GITCRYPT` header. |

## Code Examples

### AES Key Object Creation

```ts
import type { CryptoKey } from "@alloy-ts/repository";

const key: CryptoKey = {
  algorithm: { name: "AES-CBC", length: 256 },
  extractable: true,
  type: "secret",
  usages: ["encrypt", "decrypt"],
};
```

### ECDSA Key Pair Object Creation

```ts
import type { CryptoKeyPair } from "@alloy-ts/repository";

const keyPair: CryptoKeyPair = {
  publicKey: {
    algorithm: { name: "ECDSA", namedCurve: "P-521" },
    extractable: true,
    type: "public",
    usages: ["verify"],
  },
  privateKey: {
    algorithm: { name: "ECDSA", namedCurve: "P-521" },
    extractable: false,
    type: "private",
    usages: ["sign"],
  },
};
```

### Checking Algorithm Support

```ts
import { cryptoKeySupports } from "@alloy-ts/repository";

if (cryptoKeySupports("importKey", "Argon2id")) {
  console.log("Argon2id importKey is supported");
}
```
