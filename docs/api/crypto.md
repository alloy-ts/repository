# Crypto API & CryptoKey Documentation

The `crypto` module provides transparent file encryption for gix repositories and a Web Crypto API compliant [`CryptoKey`](#cryptokey) implementation backed by Rust (`blsful` provider).

---

## Web Crypto `CryptoKey` Compatibility Table

The table below tabulates the Web Crypto API / `node:crypto` specification against our native implementation.

### Key Management & Operations Support

| API Method                                                                                                             | Node.js Web Crypto API | `@alloy-ts/crypto` Implementation         | Support Status                                     |
| :--------------------------------------------------------------------------------------------------------------------- | :--------------------- | :---------------------------------------- | :------------------------------------------------- |
| `new CryptoKey(algorithm, extractable, keyUsages)`                                                                     | Supported              | Supported                                 | ✅ Fully Supported                                 |
| `CryptoKey.generateKey(algorithm, extractable, keyUsages)`                                                             | Supported              | Supported                                 | ✅ Fully Supported                                 |
| `CryptoKey.exportKey(format, key)`                                                                                     | Supported              | Supported                                 | ✅ Fully Supported (`jwk`, `raw`, `spki`, `pkcs8`) |
| `CryptoKey.importKey(format, keyData, algorithm, extractable, keyUsages)`                                              | Supported              | Supported                                 | ✅ Fully Supported                                 |
| `CryptoKey.import(format, keyData, algorithm, extractable, keyUsages)`                                                 | Supported              | Supported                                 | ✅ Fully Supported                                 |
| `CryptoKey.getPublicKey(key)`                                                                                          | Supported              | Supported                                 | ✅ Fully Supported                                 |
| `CryptoKey.encrypt(algorithm, key, data)`                                                                              | Supported              | Supported                                 | ✅ Fully Supported                                 |
| `CryptoKey.decrypt(algorithm, key, data)`                                                                              | Supported              | Supported                                 | ✅ Fully Supported                                 |
| `CryptoKey.sign(algorithm, key, data)`                                                                                 | Supported              | Supported                                 | ✅ Fully Supported                                 |
| `CryptoKey.verify(algorithm, key, signature, data)`                                                                    | Supported              | Supported                                 | ✅ Fully Supported                                 |
| `CryptoKey.deriveBits(algorithm, baseKey, length)`                                                                     | Supported              | Supported                                 | ✅ Fully Supported                                 |
| `CryptoKey.deriveKey(algorithm, baseKey, derivedKeyType, extractable, keyUsages)`                                      | Supported              | Supported                                 | ✅ Fully Supported                                 |
| `CryptoKey.wrapKey(format, key, wrappingKey, wrapAlgorithm)`                                                           | Supported              | Supported                                 | ✅ Fully Supported                                 |
| `CryptoKey.unwrapKey(format, wrappedKey, unwrappingKey, unwrapAlg, unwrappedAlg, extractable, keyUsages)`              | Supported              | Supported                                 | ✅ Fully Supported                                 |
| `CryptoKey.decapsulateBits(decapsulationAlgorithm, decapsulationKey, ciphertext)`                                      | Supported (v24.7.0+)   | Supported                                 | ✅ Supported                                       |
| `CryptoKey.decapsulateKey(decapsulationAlgorithm, decapsulationKey, ciphertext, sharedKeyAlg, extractable, keyUsages)` | Supported (v24.7.0+)   | Supported                                 | ✅ Supported                                       |
| `CryptoKey.supports(operation, algorithm, lengthOrAdditional)`                                                         | Supported              | Supported                                 | ✅ Fully Supported                                 |
| `CryptoKey.Format`                                                                                                     | Static property        | Static property (`{ format: undefined }`) | ✅ Supported                                       |

---

### Algorithm Support

| Algorithm             | Key Generation | Export / Import | Encrypt / Decrypt | Sign / Verify | Derive Bits / Key | Wrap / Unwrap Key |
| :-------------------- | :------------: | :-------------: | :---------------: | :-----------: | :---------------: | :---------------: |
| **AES-CBC**           |       ✅       |       ✅        |        ✅         |      ❌       |        ❌         |        ✅         |
| **AES-CTR**           |       ✅       |       ✅        |        ✅         |      ❌       |        ❌         |        ✅         |
| **AES-GCM**           |       ✅       |       ✅        |        ✅         |      ❌       |        ❌         |        ✅         |
| **AES-KW**            |       ✅       |       ✅        |        ❌         |      ❌       |        ❌         |        ✅         |
| **ChaCha20-Poly1305** |       ✅       |       ✅        |        ✅         |      ❌       |        ❌         |        ✅         |
| **HMAC**              |       ✅       |       ✅        |        ❌         |      ✅       |        ❌         |        ❌         |
| **ECDSA**             |       ✅       |       ✅        |        ❌         |      ✅       |        ❌         |        ❌         |
| **ECDH**              |       ✅       |       ✅        |        ❌         |      ❌       |        ✅         |        ❌         |
| **Ed25519**           |       ✅       |       ✅        |        ❌         |      ✅       |        ❌         |        ❌         |
| **X25519**            |       ✅       |       ✅        |        ❌         |      ❌       |        ✅         |        ❌         |
| **RSASSA-PKCS1-v1_5** |       ✅       |       ✅        |        ❌         |      ✅       |        ❌         |        ❌         |
| **RSA-PSS**           |       ✅       |       ✅        |        ❌         |      ✅       |        ❌         |        ❌         |
| **RSA-OAEP**          |       ✅       |       ✅        |        ✅         |      ❌       |        ❌         |        ✅         |
| **HKDF**              |       ❌       |   ✅ (`raw`)    |        ❌         |      ❌       |        ✅         |        ❌         |
| **PBKDF2**            |       ❌       |   ✅ (`raw`)    |        ❌         |      ❌       |        ✅         |        ❌         |

---

## Usage Examples

### 1. Generating Keys

```ts
import { CryptoKey } from "@alloy-ts/crypto";

// AES Symmetric Key
const aesKey = await new CryptoKey({ name: "AES-CBC", length: 256 }, true, ["encrypt", "decrypt"]);

// ECDSA Key Pair
const { publicKey, privateKey } = await new CryptoKey(
  { name: "ECDSA", namedCurve: "P-521" },
  true,
  ["sign", "verify"],
);

// Ed25519 & X25519 Keys
const ed25519Key = await new CryptoKey({ name: "Ed25519" }, true, ["sign", "verify"]);
const x25519Key = await new CryptoKey({ name: "X25519" }, true, ["deriveKey"]);

// HMAC Key
const hmacKey = await new CryptoKey({ name: "HMAC", hash: "SHA-256" }, true, ["sign", "verify"]);

// RSA Key Pair
const publicExponent = new Uint8Array([1, 0, 1]);
const rsaKey = await new CryptoKey(
  {
    name: "RSASSA-PKCS1-v1_5",
    modulusLength: 2048,
    publicExponent,
    hash: "SHA-256",
  },
  true,
  ["sign", "verify"],
);
```

### 2. Encryption and Decryption

```ts
import { CryptoKey } from "@alloy-ts/crypto";

const key = await new CryptoKey({ name: "AES-CBC", length: 256 }, true, ["encrypt", "decrypt"]);
const iv = crypto.getRandomValues(new Uint8Array(16));
const plaintext = new TextEncoder().encode("Hello, World!");

// Encrypt via instance method
const ciphertext = await key.encrypt({ name: "AES-CBC", iv }, key, plaintext);

// Decrypt via static method
const decrypted = await CryptoKey.decrypt({ name: "AES-CBC", iv }, key, ciphertext);
```

### 3. Exporting and Importing Keys

```ts
import { CryptoKey } from "@alloy-ts/crypto";

const key = await new CryptoKey({ name: "HMAC", hash: "SHA-512" }, true, ["sign", "verify"]);

// Export to JWK
const exportedJwk = await CryptoKey.exportKey("jwk", key);

// Import from JWK
const importedKey = await CryptoKey.import(
  "jwk",
  exportedJwk,
  { name: "HMAC", hash: "SHA-512" },
  true,
  ["sign", "verify"],
);
```

### 4. Wrapping and Unwrapping Keys

```ts
import { CryptoKey } from "@alloy-ts/crypto";

const key = await new CryptoKey({ name: "HMAC", hash: "SHA-512" }, true, ["sign", "verify"]);
const wrappingKey = await new CryptoKey({ name: "AES-KW", length: 256 }, true, [
  "wrapKey",
  "unwrapKey",
]);

// Wrap
const wrappedKey = await CryptoKey.wrapKey("jwk", key, wrappingKey, "AES-KW");

// Unwrap
const unwrappedKey = await CryptoKey.unwrapKey(
  "jwk",
  wrappedKey,
  wrappingKey,
  "AES-KW",
  { name: "HMAC", hash: "SHA-512" },
  true,
  ["sign", "verify"],
);
```

### 5. Runtime Support Check (`CryptoKey.supports`)

```ts
import { CryptoKey } from "@alloy-ts/crypto";

if (CryptoKey.supports("importKey", "AES-GCM")) {
  // AES-GCM import is supported
}
```
