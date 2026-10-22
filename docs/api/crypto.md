# WebCrypto API (`docs/api/crypto.md`)

This document tabulates the upstream Node.js `node:crypto` / WebCrypto API against `@alloy-ts/crypto`'s pure-Rust implementation.

## Overview

The `@alloy-ts/crypto` library provides full cross-platform WebCrypto bindings powered by pure-Rust cryptographic primitives (such as `sha2`, `sha1`, `hmac`, `hkdf`, `pbkdf2`, `aes`, `aes-gcm`, `ed25519-dalek`, `chacha20poly1305`), eliminating any C/OpenSSL build dependencies.

---

## Static Methods Comparison (`CryptoKey`)

| Upstream `node:crypto` Method | `@alloy-ts/crypto` Status | Signature / Usage |
| :--- | :--- | :--- |
| `CryptoKey.supports(operation, algorithm)` | **Supported** | `CryptoKey.supports(operation: string, algorithm: string \| AlgorithmParams): boolean` |
| `CryptoKey.importKey(format, keyData, alg, ext, usages)` | **Supported** | `CryptoKey.importKey(format: KeyFormat, keyData: Uint8Array \| string \| JWK, algorithm: AlgorithmParams, extractable: boolean, usages: KeyUsage[]): CryptoKey` |
| `CryptoKey.exportKey(format, key)` | **Supported** | `CryptoKey.exportKey(format: KeyFormat, key: CryptoKey): Uint8Array \| JWK` |
| `CryptoKey.getPublicKey(key)` | **Supported** | `CryptoKey.getPublicKey(key: CryptoKey): CryptoKey` |
| `CryptoKey.encrypt(algorithm, key, data)` | **Supported** | `CryptoKey.encrypt(algorithm: CryptoOpParams, key: CryptoKey, data: Uint8Array): Buffer` |
| `CryptoKey.decrypt(algorithm, key, data)` | **Supported** | `CryptoKey.decrypt(algorithm: CryptoOpParams, key: CryptoKey, data: Uint8Array): Buffer` |
| `CryptoKey.sign(algorithm, key, data)` | **Supported** | `CryptoKey.sign(algorithm: CryptoOpParams, key: CryptoKey, data: Uint8Array): Buffer` |
| `CryptoKey.verify(algorithm, key, sig, data)` | **Supported** | `CryptoKey.verify(algorithm: CryptoOpParams, key: CryptoKey, signature: Uint8Array, data: Uint8Array): boolean` |
| `CryptoKey.digest(algorithm, data)` | **Supported** | `CryptoKey.digest(algorithm: CryptoOpParams, data: Uint8Array): Buffer` |
| `CryptoKey.deriveBits(algorithm, baseKey, length)` | **Supported** | `CryptoKey.deriveBits(algorithm: CryptoOpParams, baseKey: CryptoKey, length?: number): Buffer` |
| `CryptoKey.deriveKey(alg, baseKey, derivedType, ext, usages)` | **Supported** | `CryptoKey.deriveKey(algorithm: CryptoOpParams, baseKey: CryptoKey, derivedKeyType: CryptoOpParams, extractable: boolean, keyUsages: KeyUsage[]): CryptoKey` |
| `CryptoKey.wrapKey(format, key, wrappingKey, alg)` | **Supported** | `CryptoKey.wrapKey(format: KeyFormat, key: CryptoKey, wrappingKey: CryptoKey, algorithm: string \| CryptoOpParams): Buffer` |
| `CryptoKey.unwrapKey(format, wrappedKey, unwrappingKey, wrapAlg, derivedAlg, ext, usages)` | **Supported** | `CryptoKey.unwrapKey(format: KeyFormat, wrappedKey: Uint8Array, unwrappingKey: CryptoKey, wrapAlgorithm: string \| CryptoOpParams, derivedKeyAlgorithm: CryptoKeyAlgorithm, extractable: boolean, keyUsages: KeyUsage[]): CryptoKey` |

---

## Instance Methods Comparison (`cryptoKey.*`)

| Method | Status | Notes |
| :--- | :--- | :--- |
| `cryptoKey.encrypt(algorithm, data)` | **Supported** | Instance method overload calling static `CryptoKey.encrypt` |
| `cryptoKey.decrypt(algorithm, data)` | **Supported** | Instance method overload calling static `CryptoKey.decrypt` |
| `cryptoKey.sign(algorithm, data)` | **Supported** | Instance method overload calling static `CryptoKey.sign` |
| `cryptoKey.verify(algorithm, signature, data)` | **Supported** | Instance method overload calling static `CryptoKey.verify` |
| `cryptoKey.digest(algorithm, data)` | **Supported** | Instance method overload calling static `CryptoKey.digest` |
| `cryptoKey.deriveBits(algorithm, length)` | **Supported** | Instance method overload calling static `CryptoKey.deriveBits` |
| `cryptoKey.deriveKey(alg, derivedType, ext, usages)` | **Supported** | Instance method overload calling static `CryptoKey.deriveKey` |
| `cryptoKey.wrapKey(format, key, alg)` | **Supported** | Instance method overload calling static `CryptoKey.wrapKey` |
| `cryptoKey.unwrapKey(format, wrappedKey, wrapAlg, derivedAlg, ext, usages)` | **Supported** | Instance method overload calling static `CryptoKey.unwrapKey` |
| `cryptoKey.publicKey` | **Supported** | Property returning the public key `CryptoKey` instance for asymmetric key pairs, or `undefined` for symmetric keys |
| `cryptoKey.privateKey` | **Supported** | Property returning the private key `CryptoKey` instance for asymmetric key pairs, or `undefined` for symmetric keys |

---

## Supported Key Formats

| Identifier | String Representation | Usage |
| :--- | :--- | :--- |
| `CryptoKey.Format.RAW` | `'raw'` | Raw symmetric key bytes or raw asymmetric public keys |
| `CryptoKey.Format.RAW_SECRET` | `'raw-secret'` | Raw secret key material |
| `CryptoKey.Format.RAW_PUBLIC` | `'raw-public'` | Raw public key material |
| `CryptoKey.Format.RAW_SEED` | `'raw-seed'` | Seed bytes for key generation |
| `CryptoKey.Format.SPKI` | `'spki'` | `SubjectPublicKeyInfo` DER/PEM formatted public keys |
| `CryptoKey.Format.PKCS8` | `'pkcs8'` | `PrivateKeyInfo` / PKCS#8 DER/PEM formatted private keys |
| `CryptoKey.Format.JWK` | `'jwk'` | JSON Web Key JavaScript Object / string |

---

## Algorithm & Operation Support Matrix

| Algorithm | Key Generation | Encrypt / Decrypt | Sign / Verify | Key Derivation | Wrap / Unwrap |
| :--- | :---: | :---: | :---: | :---: | :---: |
| **AES-CBC** | ✓ | ✓ | — | — | ✓ |
| **AES-CTR** | ✓ | ✓ | — | — | ✓ |
| **AES-GCM** | ✓ | ✓ | — | — | ✓ |
| **AES-KW** | ✓ | — | — | — | ✓ |
| **HMAC** | ✓ | — | ✓ | — | — |
| **Ed25519** | ✓ | — | ✓ | — | — |
| **ECDSA** | ✓ | — | ✓ | — | — |
| **RSASSA-PKCS1-v1_5** | ✓ | — | ✓ | — | — |
| **PBKDF2** | — | — | — | ✓ | — |
| **HKDF** | — | — | — | ✓ | — |
