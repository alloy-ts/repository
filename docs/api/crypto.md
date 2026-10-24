# Crypto API & Feature Parity with `git-crypt`

`src/crypto.rs` provides cryptographic operations and repository encryption utilities exposed as NAPI Node.js bindings.

---

## 1. Feature Parity Analysis: `src/crypto.rs` vs `git-crypt`

| Feature / Subsystem | `git-crypt` (C++) | `src/crypto.rs` (Rust / NAPI) | Parity Status & Notes |
| :--- | :--- | :--- | :--- |
| **Symmetric Encryption Engine** | AES-256 in CTR/CBC mode | ChaCha20-Poly1305 (AEAD) | **Modernized Alternative**. `src/crypto.rs` uses modern AEAD (`ChaCha20Poly1305`). Note: Native cipher payloads are not byte-compatible with upstream `git-crypt` (which uses AES-256 + HMAC-SHA1), though filter configuration (`filterName: "git-crypt"`) allows running the upstream `git-crypt` binary. |
| **HMAC & MAC** | HMAC-SHA1 | HMAC-SHA256 (via WebCrypto & `hmac`/`sha2` Rust crates) | **Modernized Alternative**. Uses SHA-256 instead of legacy SHA-1 for HMAC operations. |
| **Random Number Generation** | OpenSSL `RAND_bytes` | OS CSPRNG (`rand::rng()`, `getrandom`, `ring::rand::SystemRandom`) | **Full Parity**. Uses secure OS random number generation. |
| **Repository Initialization (`init`)** | Generates symmetric key & writes `.gitattributes` | `Crypto.prototype.init(opts?)` initializes `<git-dir>/crypt`, generates key, and configures `.git/config` filters (`filter.<name>.clean`, `smudge`, `diff`). | **Full Parity**. |
| **Symmetric Key Management (`export-key` / `import-key`)** | Key file export/import in `<git-dir>/git-crypt/keys/default` | `KeyManager` / `Crypto.prototype.exportKey()` & `importKey()` | **Full Parity**. Supports `<git-dir>/crypt/keys/default` with Unix `0600` permissions. |
| **Lock / Unlock (`lock` / `unlock`)** | Configures or removes git filters (`clean`/`smudge`/`diff`) | `Crypto.prototype.lock()` & `Crypto.prototype.unlock()` | **Full Parity**. Configures or cleans up filter entries in local git config. |
| **SSH Recipient Key Management** | Ephemeral X25519 / Ed25519 recipient wrapping | `Crypto.prototype.addSshUser()`, `importSshKey()`, `addKey()`, `SshIdentity` | **Superset Parity**. Full support for Ed25519 SSH keys, plus ref-backed key storage (`refs/keys/`) and digital signature verification. |
| **GPG Recipient Key Management (`add-gpg-user`)** | GPG/rpgp key wrapping under `crypt/keys/gpg/` | `Crypto.addGpgUser()` stub method returning actionable error | **Partial/Pending**. Keyring lookup is pending. |
| **Process / Coprocessor Execution** | Win32 `CreateProcessA` / POSIX pipe coprocessing | Tokio async process execution & streaming (`src/process.rs`) | **Full Parity**. Process execution uses native Rust async subprocessing rather than C++ Windows API calls. |
| **Web Crypto API (W3C Standard)** | N/A (not in `git-crypt`) | `CryptoKey`, `CryptoKeyPair`, `MultiSignature`, `Signature` | **Superset / Extension**. Implements Web Crypto operations (`encrypt`, `decrypt`, `sign`, `verify`, `deriveKey`, `wrapKey`, `unwrapKey`, `importKey`, `exportKey`, `JWK`/`SPKI`/`PKCS8`). |
| **XMSS^MT Stateful Signatures** | N/A (not in `git-crypt`) | `Crypto.prototype.generateXmssKey()`, `signXmss()`, `verifyXmss()` | **Superset / Extension**. Post-quantum stateful signatures with write-ahead high-water mark rollback protection. |

---

## 2. API Reference (`docs/api/crypto.md`)

### `SymmetricCryptoKey`

A 256-bit symmetric encryption key using ChaCha20-Poly1305.

```ts
class SymmetricCryptoKey {
  constructor(bytes?: Uint8Array);
  static generateRandom(): SymmetricCryptoKey;
  static fromBytes(bytes: Uint8Array): SymmetricCryptoKey;
  encrypt(plaintext: Uint8Array): Uint8Array;
  decrypt(ciphertext: Uint8Array): Uint8Array;
  static isEncrypted(data: Uint8Array): boolean;
}
```

### `Crypto`

Repository-bound encryption and recipient manager (obtained via `repo.crypto()`).

```ts
interface CryptFilterOptions {
  filterName?: string; // Default: "crypto"
  clean?: string;      // Default: "crypto clean %f"
  smudge?: string;     // Default: "crypto smudge %f"
  diff?: string;       // Default: "crypto diff"
}

class Crypto {
  isInitialized(): boolean;
  keyDir(): string;
  generateKey(): SymmetricCryptoKey;
  init(opts?: CryptFilterOptions): void;
  lock(opts?: CryptFilterOptions): void;
  unlock(keyFile?: string, opts?: CryptFilterOptions): void;
  exportKey(outputPath: string): void;
  importKey(inputPath: string): void;
  encrypt(data: Uint8Array): Uint8Array;
  decrypt(data: Uint8Array): Uint8Array;
  isEncrypted(data: Uint8Array): boolean;

  // SSH recipient methods
  addSshUser(sshKeyPath: string, alias?: string): void;
  importSshKey(encryptedPath: string, identityPath: string, passphrase?: string): void;
  addKey(name: string, keyPem: string, passphrase?: string, opts?: KeyStoreOptions): void;
  getPublicKey(name?: string): string;
  sign(data: Uint8Array, name?: string): Uint8Array;
  verify(data: Uint8Array, signature: Uint8Array, name?: string): boolean;
  listKeys(): string[];
  authenticate(identityPem: string, passphrase?: string): boolean;

  // XMSS^MT stateful post-quantum signatures
  generateXmssKey(name: string, passphrase?: string): void;
  xmssUsed(name: string): number;
  signXmss(name: string, data: Uint8Array, passphrase?: string): Uint8Array;
  verifyXmss(name: string, data: Uint8Array, signature: Uint8Array): boolean;
}
```

### `SshIdentity`

Asymmetric Ed25519 SSH identity for signing and verification.

```ts
class SshIdentity {
  constructor(pem: string, passphrase?: string);
  sign(data: Uint8Array): Uint8Array;
  verify(data: Uint8Array, signature: Uint8Array): boolean;
  static verifyWithPublicKey(pubPem: string, data: Uint8Array, signature: Uint8Array): boolean;
}
```

### `CryptoKey` (Web Crypto)

W3C WebCrypto `CryptoKey` implementation supporting AES-CBC, AES-CTR, AES-GCM, AES-KW, ChaCha20-Poly1305, HMAC, ECDSA, ECDH, Ed25519, X25519, RSA-OAEP, RSA-PSS, RSASSA-PKCS1-v1_5, PBKDF2, and HKDF.

```ts
class CryptoKey {
  constructor(algorithm: string | CryptoKeyAlgorithm, extractable: boolean, usages: string[]);
  get algorithm(): CryptoKeyAlgorithm;
  get extractable(): boolean;
  get type(): string; // "secret" | "public" | "private"
  get usages(): string[];
  get publicKey(): CryptoKey | undefined;
  get privateKey(): CryptoKey | undefined;

  static importKey(format: string, keyData: Uint8Array | object, algorithm: string | CryptoKeyAlgorithm, extractable: boolean, usages: string[]): CryptoKey;
  static exportKey(format: string, key: CryptoKey): Uint8Array | object;
  static getPublicKey(key: CryptoKey): CryptoKey;
  static supports(operation: string, algorithm: string | CryptoOpParams): boolean;

  static encrypt(algorithm: CryptoOpParams, key: CryptoKey, data: Uint8Array): Uint8Array;
  static decrypt(algorithm: CryptoOpParams, key: CryptoKey, data: Uint8Array): Uint8Array;
  static sign(algorithm: CryptoOpParams, key: CryptoKey, data: Uint8Array): Uint8Array;
  static verify(algorithm: CryptoOpParams, key: CryptoKey, signature: Uint8Array, data: Uint8Array): boolean;
  static deriveBits(algorithm: CryptoOpParams, baseKey: CryptoKey, length?: number): Uint8Array;
  static deriveKey(algorithm: CryptoOpParams, baseKey: CryptoKey, derivedKeyType: CryptoOpParams, extractable: boolean, keyUsages: string[]): CryptoKey;
  static digest(algorithm: CryptoOpParams, data: Uint8Array): Uint8Array;
  static wrapKey(format: string, key: CryptoKey, wrappingKey: CryptoKey, algorithm: string | CryptoOpParams): Uint8Array;
  static unwrapKey(format: string, wrappedKey: Uint8Array, unwrappingKey: CryptoKey, wrapAlgorithm: string | CryptoOpParams, derivedKeyAlgorithm: CryptoKeyAlgorithm, extractable: bool, keyUsages: string[]): CryptoKey;
}
```
