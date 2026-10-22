# Crypto API Documentation

`src/crypto.rs` provides cryptographic operations, Web Crypto API `CryptoKey` bindings, format-aware `Signature` objects, and `Crypto` transparent file encryption handles for git repositories.

---

## Overview & Architecture

The crypto subsystem consists of four primary components exposed via NAPI bindings:

1. **Web Crypto `CryptoKey`**:
   Node.js Web Crypto compatible symmetric key generation, asymmetric key pair generation, key import/export, encryption/decryption, signing/verification, key wrapping/unwrapping, key derivation (PBKDF2/HKDF), digest, and feature detection (`CryptoKey.supports`).

2. **Format-Aware `Signature`**:
   Tagged cryptographic signature container associated with a specific key format from `CryptoKey.Format` (e.g. `raw`, `spki`, `pkcs8`, `jwk`, `raw-secret`, `raw-public`, `raw-seed`).

3. **Multisignature (`MultiSignature`, `SignatureBuilder`, `SignatureBytes`, `SignatureScheme`)**:
   Self-describing multisignatures backed by `multi-sig`, `multi-codec`, and `multi-key`.

4. **Repository Cryptography (`Crypto`, `CryptKey`, `SshIdentity`)**:
   Per-repository transparent payload encryption (`git-crypt` compatible clean/smudge/diff filters), SSH recipient key vault management, and XMSS^MT stateful post-quantum signatures.

---

## Method Mapping: Upstream Node `node:crypto` / Web Crypto vs. Our Implementation

Below is a detailed mapping tabulating upstream Node `node:crypto` / Web Crypto API methods against our native NAPI implementation in `src/crypto.rs`.

### Class: `CryptoKey`

| Operation / Method | Node `node:crypto` / Web Crypto API | `src/crypto.rs` Implementation | Notes & Algorithm Support |
| :--- | :--- | :--- | :--- |
| **Constructor** | `new CryptoKey(algorithm, extractable, keyUsages)` | `new CryptoKey(algorithm, extractable, usages)` | Generates symmetric keys (AES-CBC, AES-CTR, AES-GCM, AES-KW, HMAC) or asymmetric key pairs (ECDSA, ECDH, Ed25519, X25519, RSASSA-PKCS1-v1_5, RSA-PSS, RSA-OAEP). |
| **`CryptoKey.Format`** | Static property / enum | `CryptoKey.Format` | Static getter returning `{ RAW: "raw", RAW_SECRET: "raw-secret", RAW_PUBLIC: "raw-public", RAW_SEED: "raw-seed", SPKI: "spki", PKCS8: "pkcs8", JWK: "jwk" }`. |
| **`CryptoKey.supports`** | `CryptoKey.supports(operation, algorithm[, lengthOrAdditionalAlgorithm])` | `CryptoKey.supports(operation, algorithm[, lengthOrAdditionalAlgorithm])` | Feature detection for runtime algorithm support across Web Crypto operations. |
| **`CryptoKey.importKey`** | `CryptoKey.importKey(format, keyData, algorithm, extractable, keyUsages)` | `CryptoKey.importKey(format, keyData, algorithm, extractable, usages)` | Static method supporting `raw`, `raw-secret`, `raw-public`, `spki`, `pkcs8`, and `jwk` formats. |
| **`CryptoKey.exportKey`** | `CryptoKey.exportKey(format, key)` | `CryptoKey.exportKey(format, key)` | Static method exporting key material to `Buffer` or `JWK` JSON object. |
| **`cryptoKey.exportKey`** | Instance form `key.exportKey(format)` | `key.exportKeyInstance(format)` | Instance method form of key export. |
| **`CryptoKey.getPublicKey`**| `CryptoKey.getPublicKey(key)` | `CryptoKey.getPublicKey(key)` | Extract public key from asymmetric private key. |
| **`cryptoKey.getPublicKey`**| Instance form `key.getPublicKey()` | `key.getPublicKeyInstance()` | Instance method form of public key extraction. |
| **`CryptoKey.encrypt`** | `CryptoKey.encrypt(algorithm, key, data)` | `CryptoKey.encrypt(algorithm, key, data)` | Static method supporting `AES-CBC`, `AES-CTR`, `AES-GCM`, `AES-OCB`, `ChaCha20-Poly1305`, and `RSA-OAEP`. |
| **`cryptoKey.encrypt`** | `key.encrypt(algorithm, key, data)` | `key.encrypt(algorithm, key, data)` | Instance method form of encryption. |
| **`CryptoKey.decrypt`** | `CryptoKey.decrypt(algorithm, key, data)` | `CryptoKey.decrypt(algorithm, key, data)` | Static method supporting decipherment of encrypted payloads. |
| **`cryptoKey.decrypt`** | `key.decrypt(algorithm, key, data)` | `key.decrypt(algorithm, key, data)` | Instance method form of decryption. |
| **`CryptoKey.sign`** | `CryptoKey.sign(algorithm, key, data)` | `CryptoKey.sign(algorithm, key, data)` | Static signature generation supporting `ECDSA`, `Ed25519`, `HMAC`, `KMAC128`, `KMAC256`, `RSA-PSS`, `RSASSA-PKCS1-v1_5`. |
| **`cryptoKey.sign`** | `key.sign(algorithm, key, data)` | `key.sign(algorithm, key, data)` | Instance method form of signature generation. |
| **`CryptoKey.verify`** | `CryptoKey.verify(algorithm, key, signature, data)` | `CryptoKey.verify(algorithm, key, signature, data)` | Static signature verification. |
| **`cryptoKey.verify`** | `key.verify(algorithm, key, signature, data)` | `key.verify(algorithm, key, signature, data)` | Instance method form of signature verification. |
| **`CryptoKey.deriveBits`** | `CryptoKey.deriveBits(algorithm, baseKey, length)` | `CryptoKey.deriveBits(algorithm, baseKey, length)` | Static raw bit derivation supporting `PBKDF2`, `HKDF`, `ECDH`, `X25519`, `Argon2id`. |
| **`cryptoKey.deriveBits`** | `key.deriveBits(algorithm, baseKey, length)` | `key.deriveBits(algorithm, baseKey, length)` | Instance method form of deriveBits. |
| **`CryptoKey.deriveKey`** | `CryptoKey.deriveKey(algorithm, baseKey, derivedKeyType, extractable, keyUsages)` | `CryptoKey.deriveKey(algorithm, baseKey, derivedKeyType, extractable, keyUsages)` | Static key derivation returning a new `CryptoKey`. |
| **`cryptoKey.deriveKey`** | `key.deriveKey(...)` | `key.deriveKey(...)` | Instance method form of deriveKey. |
| **`CryptoKey.wrapKey`** | `CryptoKey.wrapKey(format, key, wrappingKey, wrapAlgorithm)` | `CryptoKey.wrapKey(format, key, wrappingKey, algorithm)` | Static key wrapping supporting `AES-KW`, `AES-GCM`, `AES-CBC`, `RSA-OAEP`. |
| **`cryptoKey.wrapKey`** | `key.wrapKey(...)` | `key.wrapKey(...)` | Instance method form of key wrapping. |
| **`CryptoKey.unwrapKey`** | `CryptoKey.unwrapKey(format, wrappedKey, unwrappingKey, wrapAlgorithm, derivedKeyAlgorithm, extractable, keyUsages)` | `CryptoKey.unwrapKey(format, wrappedKey, unwrappingKey, wrapAlgorithm, derivedKeyAlgorithm, extractable, keyUsages)` | Static key unwrapping into a new `CryptoKey`. |
| **`cryptoKey.unwrapKey`** | `key.unwrapKey(...)` | `key.unwrapKey(...)` | Instance method form of key unwrapping. |
| **`CryptoKey.digest`** | `CryptoKey.digest(algorithm, data)` | `CryptoKey.digest(algorithm, data)` | Static hash digest (`SHA-1`, `SHA-256`, `SHA-384`, `SHA-512`). |
| **`cryptoKey.digest`** | `key.digest(algorithm, data)` | `key.digest(algorithm, data)` | Instance method form of digest. |
| **Post-Quantum KEMs** | `encapsulateBits`, `decapsulateBits`, `encapsulateKey`, `decapsulateKey` | `encapsulateBits`, `decapsulateBits`, `encapsulateKey`, `decapsulateKey` | Surface methods present; throws explicit "not implemented in this build" error for post-quantum KEMs (`ML-KEM-*`). |

---

### Class: `Signature`

Format-aware cryptographic signature object backing format-tagged signature values.

| Property / Method | Type / Signature | Description |
| :--- | :--- | :--- |
| **Constructor** | `new Signature(keyFormat: string)` | Constructs an empty signature tagged with `keyFormat` (e.g. `CryptoKey.Format.RAW`). |
| **`keyFormat`** | Getter (`string`) | Returns the format identifier this signature is tagged with. |
| **`bytes`** | Getter (`Buffer`) | Returns the raw signature bytes. |
| **`setBytes`** | `(bytes: Buffer) => void` | Replaces the signature bytes. |
| **`length`** | Getter (`u32`) | Returns length in bytes. |
| **`isEmpty`** | Getter (`boolean`) | Returns `true` if signature contains 0 bytes. |
| **`hex`** | Getter (`string`) | Returns lowercase hex-encoded representation of the signature bytes. |

---

## Code Examples

### 1. Generating Symmetric & Asymmetric Keys

```ts
import { CryptoKey } from "@alloy-ts/crypto";

// AES-CBC Symmetric key
const aesKey = await new CryptoKey({ name: "AES-CBC", length: 256 }, true, ["encrypt", "decrypt"]);

// ECDSA Key Pair
const ecKeyPair = await new CryptoKey({ name: "ECDSA", namedCurve: "P-521" }, true, ["sign", "verify"]);
const { publicKey, privateKey } = ecKeyPair;

// Ed25519 Key Pair
const edKeyPair = await new CryptoKey({ name: "Ed25519" }, true, ["sign", "verify"]);

// HMAC Key
const hmacKey = await new CryptoKey({ name: "HMAC", hash: { name: "SHA-256" } }, true, ["sign", "verify"]);

// RSASSA-PKCS1-v1_5 Key Pair
const rsaKeyPair = await new CryptoKey(
  { name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: { name: "SHA-256" } },
  true,
  ["sign", "verify"]
);
```

### 2. Encryption, Decryption, and Signature Objects

```ts
import { CryptoKey, Signature } from "@alloy-ts/crypto";

// Symmetric AES-GCM Encrypt & Decrypt
const key = new CryptoKey({ name: "AES-GCM", length: 256 }, true, ["encrypt", "decrypt"]);
const iv = crypto.getRandomValues(new Uint8Array(12));
const plaintext = new TextEncoder().encode("Hello, Web Crypto!");

const ciphertext = await CryptoKey.encrypt({ name: "AES-GCM", iv }, key, plaintext);
const decrypted = await CryptoKey.decrypt({ name: "AES-GCM", iv }, key, ciphertext);

// Format-aware Signature usage
const sig = new Signature(CryptoKey.Format.RAW);
sig.setBytes(ciphertext);
console.log(sig.keyFormat); // "raw"
console.log(sig.hex);       // lowercase hex representation
```

### 3. Key Wrapping and Unwrapping

```ts
import { CryptoKey } from "@alloy-ts/crypto";

const wrappingKey = new CryptoKey({ name: "AES-KW", length: 256 }, true, ["wrapKey", "unwrapKey"]);
const targetKey = new CryptoKey({ name: "HMAC", hash: { name: "SHA-512" } }, true, ["sign", "verify"]);

// Wrap key
const wrapped = await CryptoKey.wrapKey("jwk", targetKey, wrappingKey, "AES-KW");

// Unwrap key
const unwrapped = await CryptoKey.unwrapKey(
  "jwk",
  wrapped,
  wrappingKey,
  "AES-KW",
  { name: "HMAC", hash: { name: "SHA-512" } },
  true,
  ["sign", "verify"]
);
```
