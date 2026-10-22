# WebCrypto & Node:Crypto API Implementation Status

This document tabulates the upstream `node:crypto` / WebCrypto API against our NAPI bindings implementation (`@alloy-ts/crypto` / `repository`).

## Key Management APIs

| Upstream `node:crypto` / WebCrypto API | `@alloy-ts/crypto` Implementation | Status |
| --- | --- | --- |
| `new CryptoKey(algorithm, extractable, usages)` | `new CryptoKey(algorithm, extractable, usages)` | ✅ Implemented |
| `{ publicKey, privateKey } = key` | `key.publicKey`, `key.privateKey` | ✅ Implemented |
| `CryptoKey.supports(operation, algorithm)` | `CryptoKey.supports(operation, algorithm)` | ✅ Implemented |
| `CryptoKey.exportKey(format, key)` | `CryptoKey.exportKey(format, key)` | ⚠️ Error stub |
| `CryptoKey.importKey(format, keyData, alg, extractable, usages)` | `CryptoKey.importKey(...)` | ⚠️ Error stub |
| `CryptoKey.deriveKey(alg, baseKey, derivedType, extractable, usages)` | `CryptoKey.deriveKey(...)` | ✅ Implemented (returns CryptoKey) |
| `CryptoKey.deriveBits(alg, baseKey, length)` | `CryptoKey.deriveBits(...)` | ⚠️ Error stub |
| `CryptoKey.wrapKey(format, key, wrappingKey, wrapAlg)` | `CryptoKey.wrapKey(...)` | ⚠️ Error stub |
| `CryptoKey.unwrapKey(...)` | `CryptoKey.unwrapKey(...)` | ✅ Implemented (returns CryptoKey) |

## Crypto Operation APIs

| Upstream `node:crypto` / WebCrypto API | `@alloy-ts/crypto` Implementation | Status |
| --- | --- | --- |
| `cryptoKey.encrypt(algorithm, key, data)` | `cryptoKey.encrypt(algorithm, data)` | ⚠️ Error stub |
| `cryptoKey.decrypt(algorithm, key, data)` | `cryptoKey.decrypt(algorithm, data)` | ⚠️ Error stub |
| `cryptoKey.sign(algorithm, key, data)` | `cryptoKey.sign(algorithm, data)` | ⚠️ Error stub |
| `cryptoKey.verify(algorithm, key, signature, data)` | `cryptoKey.verify(algorithm, signature, data)` | ⚠️ Error stub |
| `cryptoKey.decapsulateBits(alg, ciphertext)` | `cryptoKey.decapsulateBits(alg, ciphertext)` | ⚠️ Error stub |
| `cryptoKey.decapsulateKey(...)` | `cryptoKey.decapsulateKey(...)` | ⚠️ Error stub |

## Key Formats (`isFormatSupported`)

| Format Identifier | Supported Algorithms | Status |
| --- | --- | --- |
| `jwk`, `raw`, `raw-secret` | `AES-CBC`, `AES-CTR`, `AES-GCM`, `AES-KW`, `HMAC` | ✅ Supported |
| `jwk`, `raw-secret` | `AES-OCB`, `ChaCha20-Poly1305`, `KMAC128`, `KMAC256` | ✅ Supported |
| `raw`, `raw-secret` (import only) | `Argon2d`, `Argon2i`, `Argon2id`, `HKDF`, `PBKDF2` | ✅ Supported |
| `spki`, `pkcs8`, `jwk`, `raw`, `raw-public` | `ECDH`, `ECDSA`, `Ed25519`, `Ed448`, `X25519`, `X448` | ✅ Supported |
| `spki`, `pkcs8`, `jwk` | `RSA-OAEP`, `RSA-PSS`, `RSASSA-PKCS1-v1_5` | ✅ Supported |
