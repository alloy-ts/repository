# Crypto & CryptoKey API

The `@alloy-ts/crypto` module exports Web Crypto `CryptoKey` bindings and repository-level transparent file encryption (`Crypto`).

`CryptoKey` provides an implementation of the Web Crypto API `CryptoKey` specification for key generation, import/export, wrap/unwrap, key derivation, signature operations, key encapsulation, and algorithm feature detection.

---

## Web Crypto `node:crypto` vs `@alloy-ts/crypto` Feature Matrix

The table below compares the standard Node.js Web Crypto API (`node:crypto` / `globalThis.crypto.subtle`) with `@alloy-ts/crypto`'s `CryptoKey` implementation.

### Key Management APIs

| Method / API                                                              | Node.js `node:crypto` / Web Crypto                        | `@alloy-ts/crypto` Support                             | Return Type                  | Supported Algorithms & Formats                                                                                                                                               |
| ------------------------------------------------------------------------- | --------------------------------------------------------- | ------------------------------------------------------ | ---------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `new CryptoKey(algorithm, extractable, keyUsages)`                        | Generates a new symmetric key or asymmetric key pair      | Supported (Constructor & Callable)                     | `CryptoKey \| CryptoKeyPair` | `AES-CBC`, `AES-CTR`, `AES-GCM`, `AES-KW`, `AES-OCB`, `ChaCha20-Poly1305`, `HMAC`, `ECDSA`, `ECDH`, `Ed25519`, `X25519`, `RSASSA-PKCS1-v1_5`, `RSA-PSS`, `RSA-OAEP`          |
| `CryptoKey.importKey(format, keyData, algorithm, extractable, keyUsages)` | Imports a key from binary or JWK format                   | Supported (`CryptoKey.importKey` & `CryptoKey.import`) | `CryptoKey`                  | Formats: `raw`, `raw-secret`, `raw-public`, `raw-seed`, `spki`, `pkcs8`, `jwk`                                                                                               |
| `CryptoKey.exportKey(format, key)`                                        | Exports key material into a standard format               | Supported                                              | `ArrayBuffer` / `Buffer`     | Formats: `raw`, `raw-secret`, `raw-public`, `raw-seed`, `spki`, `pkcs8`, `jwk`                                                                                               |
| `CryptoKey.supports(operation, algorithm, lengthOrAdditional?)`           | Checks runtime support for an algorithm & operation       | Supported                                              | `boolean`                    | Detects support for `encrypt`, `decrypt`, `sign`, `verify`, `deriveKey`, `deriveBits`, `wrapKey`, `unwrapKey`, `importKey`, `exportKey`, `decapsulateBits`, `decapsulateKey` |
| `CryptoKey.Format`                                                        | Enum of key formats (`spki`, `pkcs8`, `jwk`, `raw`, etc.) | Supported                                              | `CryptoKeyFormat` enum       | `CryptoKey.Format.Raw`, `RawSecret`, `RawPublic`, `RawSeed`, `Pkcs8`, `Spki`, `Jwk`                                                                                          |

### Cryptographic Operations

| Operation Method                                                                   | Node.js `node:crypto`                        | `@alloy-ts/crypto` Static                          | `@alloy-ts/crypto` Instance Method      | Supported Algorithms                                                             |
| ---------------------------------------------------------------------------------- | -------------------------------------------- | -------------------------------------------------- | --------------------------------------- | -------------------------------------------------------------------------------- |
| `encrypt(algorithm, key, data)`                                                    | `crypto.subtle.encrypt`                      | `CryptoKey.encrypt(alg, key, data)`                | `cryptoKey.encrypt(alg, key, data)`     | `AES-CBC`, `AES-CTR`, `AES-GCM`, `AES-OCB`, `ChaCha20-Poly1305`, `RSA-OAEP`      |
| `decrypt(algorithm, key, data)`                                                    | `crypto.subtle.decrypt`                      | `CryptoKey.decrypt(alg, key, data)`                | `cryptoKey.decrypt(alg, key, data)`     | `AES-CBC`, `AES-CTR`, `AES-GCM`, `AES-OCB`, `ChaCha20-Poly1305`, `RSA-OAEP`      |
| `sign(algorithm, key, data)`                                                       | `crypto.subtle.sign`                         | `CryptoKey.sign(alg, key, data)`                   | `cryptoKey.sign(alg, key, data)`        | `ECDSA`, `Ed25519`, `HMAC`, `KMAC128`, `KMAC256`, `RSA-PSS`, `RSASSA-PKCS1-v1_5` |
| `verify(algorithm, key, signature, data)`                                          | `crypto.subtle.verify`                       | `CryptoKey.verify(alg, key, sig, data)`            | `cryptoKey.verify(alg, key, sig, data)` | `ECDSA`, `Ed25519`, `HMAC`, `KMAC128`, `KMAC256`, `RSA-PSS`, `RSASSA-PKCS1-v1_5` |
| `wrapKey(format, key, wrappingKey, wrapAlg)`                                       | `crypto.subtle.wrapKey`                      | `CryptoKey.wrapKey(format, key, wrappingKey, alg)` | `wrappingKey.wrapKey(...)`              | `AES-KW`, `AES-GCM`, `AES-CBC`, `RSA-OAEP`                                       |
| `unwrapKey(format, wrappedKey, wrappingKey, unwrapAlg, unwrappedAlg, ext, usages)` | `crypto.subtle.unwrapKey`                    | `CryptoKey.unwrapKey(...)`                         | `wrappingKey.unwrapKey(...)`            | `AES-KW`, `AES-GCM`, `AES-CBC`, `RSA-OAEP`                                       |
| `deriveKey(algorithm, baseKey, derivedType, ext, usages)`                          | `crypto.subtle.deriveKey`                    | `CryptoKey.deriveKey(...)`                         | `baseKey.deriveKey(...)`                | `Argon2id`, `Argon2i`, `Argon2d`, `PBKDF2`, `HKDF`, `ECDH`, `X25519`             |
| `deriveBits(algorithm, baseKey, length)`                                           | `crypto.subtle.deriveBits`                   | `CryptoKey.deriveBits(...)`                        | `baseKey.deriveBits(...)`               | `Argon2id`, `Argon2i`, `Argon2d`, `PBKDF2`, `HKDF`, `ECDH`, `X25519`             |
| `decapsulateBits(decapsulationAlg, key, ciphertext)`                               | `crypto.subtle.decapsulateBits` (Node 24.7+) | `CryptoKey.decapsulateBits(...)`                   | `key.decapsulateBits(...)`              | `ML-KEM-512`, `ML-KEM-768`, `ML-KEM-1024`, `MLKEM768-P256`, `MLKEM768-X25519`    |
| `decapsulateKey(decapsulationAlg, key, ciphertext, sharedAlg, ext, usages)`        | `crypto.subtle.decapsulateKey` (Node 24.7+)  | `CryptoKey.decapsulateKey(...)`                    | `key.decapsulateKey(...)`               | `ML-KEM-512`, `ML-KEM-768`, `ML-KEM-1024`, `MLKEM768-P256`, `MLKEM768-X25519`    |

---

## Key Formats (`CryptoKey.Format`)

`@alloy-ts/crypto` supports standard Web Crypto key formats accessible via `CryptoKey.Format`:

| Format Enum                  | String Equivalent | Description                         | Supported Key Types                                   |
| ---------------------------- | ----------------- | ----------------------------------- | ----------------------------------------------------- |
| `CryptoKey.Format.Raw`       | `'raw'`           | Raw unformatted key material bytes  | AES, HMAC, HKDF, PBKDF2, ECDSA, ECDH, Ed25519, X25519 |
| `CryptoKey.Format.RawSecret` | `'raw-secret'`    | Raw secret key material bytes       | AES, HMAC, ChaCha20-Poly1305, Argon2id                |
| `CryptoKey.Format.RawPublic` | `'raw-public'`    | Raw uncompressed public key bytes   | ECDSA, ECDH, Ed25519, X25519, ML-DSA                  |
| `CryptoKey.Format.RawSeed`   | `'raw-seed'`      | Raw seed bytes                      | ML-KEM, ML-DSA                                        |
| `CryptoKey.Format.Spki`      | `'spki'`          | SubjectPublicKeyInfo DER/PEM format | ECDSA, ECDH, Ed25519, X25519, RSA                     |
| `CryptoKey.Format.Pkcs8`     | `'pkcs8'`         | PrivateKeyInfo DER/PEM format       | ECDSA, ECDH, Ed25519, X25519, RSA                     |
| `CryptoKey.Format.Jwk`       | `'jwk'`           | JSON Web Key dictionary             | All supported key types                               |

---

## Examples & Usage Guide

### 1. Generating Symmetric & Asymmetric Keys

```ts
import { CryptoKey } from "@alloy-ts/crypto";

// AES symmetric key
async function newAesKey(length = 256) {
  const key = await new CryptoKey({ name: "AES-CBC", length }, true, ["encrypt", "decrypt"]);
  return key;
}

// ECDSA key pair
async function newEcKey(namedCurve = "P-521") {
  const { publicKey, privateKey } = await new CryptoKey({ name: "ECDSA", namedCurve }, true, [
    "sign",
    "verify",
  ]);
  return { publicKey, privateKey };
}

// Ed25519 and X25519 key pairs
async function newEd25519Key() {
  return await new CryptoKey({ name: "Ed25519" }, true, ["sign", "verify"]);
}

async function newX25519Key() {
  return await new CryptoKey({ name: "X25519" }, true, ["deriveKey"]);
}

// HMAC key
async function newHmacKey(hash = "SHA-256") {
  return await new CryptoKey({ name: "HMAC", hash }, true, ["sign", "verify"]);
}

// RSA key pair
async function newRsaKey(modulusLength = 2048, hash = "SHA-256") {
  const { publicKey, privateKey } = await new CryptoKey(
    {
      name: "RSASSA-PKCS1-v1_5",
      modulusLength,
      publicExponent: new Uint8Array([1, 0, 1]),
      hash,
    },
    true,
    ["sign", "verify"],
  );
  return { publicKey, privateKey };
}
```

### 2. Encryption and Decryption

```ts
import { CryptoKey } from "@alloy-ts/crypto";

async function aesEncrypt(plaintext: string) {
  const ec = new TextEncoder();
  const cryptoKey = await new CryptoKey({ name: "AES-CBC", length: 256 }, true, [
    "encrypt",
    "decrypt",
  ]);
  const iv = crypto.getRandomValues(new Uint8Array(16));

  const ciphertext = await cryptoKey.encrypt(
    { name: "AES-CBC", iv },
    cryptoKey,
    ec.encode(plaintext),
  );

  return { cryptoKey, iv, ciphertext };
}

async function aesDecrypt(ciphertext: Buffer, cryptoKey: any, iv: Uint8Array) {
  const dec = new TextDecoder();
  const plaintext = await cryptoKey.decrypt({ name: "AES-CBC", iv }, cryptoKey, ciphertext);

  return dec.decode(plaintext);
}
```

### 3. Exporting, Importing, Wrapping & Unwrapping Keys

```ts
import { CryptoKey } from "@alloy-ts/crypto";

// Export and Import
async function newAndExportHmacKey(format = "jwk", hash = "SHA-512") {
  const key = await new CryptoKey({ name: "HMAC", hash }, true, ["sign", "verify"]);
  return CryptoKey.exportKey(format, key);
}

async function importHmacKey(keyData: Buffer, format = "jwk", hash = "SHA-512") {
  return await CryptoKey.import(format, keyData, { name: "HMAC", hash }, true, ["sign", "verify"]);
}

// Wrap and Unwrap
async function wrapKeyExample() {
  const key = await new CryptoKey({ name: "HMAC" }, true, ["sign", "verify"]);
  const wrappingKey = await new CryptoKey({ name: "AES-KW", length: 256 }, true, [
    "wrapKey",
    "unwrapKey",
  ]);

  const wrappedKey = await CryptoKey.wrapKey("jwk", key, wrappingKey, "AES-KW");
  const unwrappedKey = await CryptoKey.unwrapKey(
    "jwk",
    wrappedKey,
    wrappingKey,
    "AES-KW",
    { name: "HMAC" },
    true,
    ["sign", "verify"],
  );

  return unwrappedKey;
}
```

### 4. Feature Detection with `CryptoKey.supports`

```ts
import { CryptoKey } from "@alloy-ts/crypto";

const isArgonSupported = CryptoKey.supports("importKey", "Argon2id");
const isAesOcbSupported = CryptoKey.supports("importKey", "AES-OCB");

console.log(`Argon2id support: ${isArgonSupported}`);
console.log(`AES-OCB support: ${isAesOcbSupported}`);
```

---

## Repository File Encryption (`Crypto`)

`Repository.crypt()` provides transparent repository file encryption ("git-crypt" style):

```ts
import { Repository } from "@alloy-ts/repository";

const repo = Repository.open(".");
const crypt = repo.crypt();

// Initialize repository crypt key (.git/crypt/keys/default)
if (!crypt.isInitialized()) {
  crypt.init();
}

// Encrypt and decrypt buffers
const encrypted = crypt.encrypt(Buffer.from("sensitive data"));
console.log(crypt.isEncrypted(encrypted)); // true

const decrypted = crypt.decrypt(encrypted);
console.log(decrypted.toString()); // "sensitive data"
```
