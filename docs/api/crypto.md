# Crypto / Web Crypto API Bindings

NAPI bindings for transparent repository file encryption (`Crypt`) and Web Crypto `CryptoKey` / `CryptoKeyPair` key representations.

## `CryptoKey` & `CryptoKeyPair` Object Bindings

The Web Crypto `CryptoKey` interface represents a cryptographic key (symmetric secret key or asymmetric public/private key).

```ts
import {
  CryptoKey,
  CryptoKeyPair,
  CryptoKeyFormat,
  Signature,
  createCryptoKey,
  createCryptoKeyPair,
  cryptoSign,
  cryptoVerify,
  cryptoDeriveBits,
  cryptoDeriveKey,
  cryptoGetPublicKey,
  cryptoSupports,
} from "repository";
```

### Type Mapping & Key Algorithms

| Upstream `node:crypto` / WebCrypto | Our Implementation (`repository`) | Notes / Supported Algorithms |
| ---------------------------------- | --------------------------------- | ---------------------------- |
| `CryptoKey`                        | `CryptoKey` (interface/object)    | `{ algorithm, extractable, type, usages }` |
| `CryptoKeyPair`                    | `CryptoKeyPair` (interface)       | `{ publicKey, privateKey }` |
| `KeyAlgorithm`                     | `KeyAlgorithm`                    | `{ name: string }` |
| `AesKeyAlgorithm`                  | `AesKeyAlgorithm`                 | `{ name: string, length: number }` (128, 192, 256 bits) |
| `EcKeyAlgorithm`                   | `EcKeyAlgorithm`                  | `{ name: string, namedCurve: string }` (`P-256`, `P-384`, `P-521`) |
| `HmacKeyAlgorithm`                 | `HmacKeyAlgorithm`                | `{ name: string, length?: number, hash?: KeyAlgorithm }` |
| `RsaHashedKeyAlgorithm`            | `RsaHashedKeyAlgorithm`           | `{ name: string, modulusLength: number, publicExponent: Buffer, hash: KeyAlgorithm }` |
| `KmacKeyAlgorithm`                 | `KmacKeyAlgorithm`                | `{ name: string, length?: number }` |

### Comparison: Key Management & Cryptographic Operations

| Operation / Method | Upstream `node:crypto` | Our Implementation | Signature / Notes |
| ------------------ | ---------------------- | ------------------ | ----------------- |
| Key creation       | `new CryptoKey(...)` / `subtle.generateKey` | `createCryptoKey(...)` / `createCryptoKeyPair(...)` | Create typed NAPI `CryptoKey` or `CryptoKeyPair` object |
| Signature format   | `CryptoKey.Format`     | `CryptoKeyFormat` / `Signature` | Key format enum (`Jwk`, `Raw`, `Spki`, `Pkcs8`, `RawSecret`, `RawPublic`, `RawSeed`) |
| Signing            | `subtle.sign`          | `cryptoSign`       | `(algorithm, key, data) => Buffer` |
| Verification       | `subtle.verify`        | `cryptoVerify`     | `(algorithm, key, signature, data) => boolean` |
| Derive bits        | `subtle.deriveBits`    | `cryptoDeriveBits` | `(algorithm, baseKey, length?) => Buffer` |
| Derive key         | `subtle.deriveKey`     | `cryptoDeriveKey`  | `(algorithm, baseKey, derivedKeyType, extractable, usages) => CryptoKey` |
| Get public key     | `CryptoKey.getPublicKey` | `cryptoGetPublicKey` | `(pair: CryptoKeyPair) => CryptoKey` |
| Feature detection  | `CryptoKey.supports`   | `cryptoSupports`   | `(operation, algorithm, extra?) => boolean` |

## Repository Crypt Subsystem (`Crypt`)

The `Crypt` handle manages transparent per-repository encryption (`filter.git-crypt` filters and `.git/crypt/keys/default`).

| Method          | Signature                             | Description |
| --------------- | ------------------------------------- | ----------- |
| `isInitialized` | `() => boolean`                       | Whether `.git/crypt` directory and default key exist. |
| `init`          | `() => void`                          | Initialize crypt directory, generate symmetric key, configure git filters. |
| `lock`          | `() => void`                          | Lock repository by removing git filter configuration. |
| `unlock`        | `(keyFile?: string) => void`          | Unlock repository using local key or imported key file. |
| `exportKey`     | `(outputPath: string) => void`        | Export default symmetric key to file path (`0600` permissions on Unix). |
| `importKey`     | `(inputPath: string) => void`         | Import symmetric key from file path as default key. |
| `encrypt`       | `(data: Buffer) => Buffer`            | Encrypt buffer payload with ChaCha20-Poly1305. |
| `decrypt`       | `(data: Buffer) => Buffer`            | Decrypt buffer payload. |
| `isEncrypted`   | `(data: Buffer) => boolean`           | Check whether buffer carries the `GITCRYPT` magic header. |
