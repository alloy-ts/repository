# Cryptography and GPG API

The cryptography subsystem provides symmetric encryption, repository vault key
management, and GPG / OpenPGP operations backed by `rPGP`.

## Overview

The cryptography capabilities in `@alloy-ts/repository` consist of two primary
areas:

1. **Symmetric Repository Encryption & Key Vault (`src/crypto.rs`)**:
   - `CryptoMode`: Modes for repository encryption — `Filter` (0), `EntireRepo`
     (1), and default `Hybrid` (2).
   - `SymmetricCryptoKey`: ChaCha20-Poly1305 / AES 256-bit symmetric
     encryption/decryption, raw byte access (`asBytes`), and random key
     generation.
   - `Crypto`: Repository-bound crypt handle managing `git-crypt` worktree
     filter drivers, `git-remote-gcrypt` compatible entire repository
     encryption, manifest creation/decryption, and packfile AES /
     ChaCha20-Poly1305 payload encryption.
   - `RepoManifest` & Manifest Functions: `serialiseManifest` and
     `parseManifest` for `git-remote-gcrypt` manifest handling (refs, packs,
     keep list, remote repository ID).
   - `CryptFilterOptions`: Configuration for `clean`, `smudge`, and `diff` Git
     filters.

2. **OpenPGP & GPG Keyring Operations (`src/gpg.rs`)**:
   - `Context`: High-level cryptographic operations including encryption,
     decryption, signing, and signature verification.
   - `Key`: Inspection of OpenPGP public and secret keys (key ID, fingerprint,
     user IDs, secret status).
   - `Data`: Wrapper for plaintext or ciphertext buffers, supporting binary and
     ASCII armor forms.
   - `VerificationResult`: Structured signature verification results.
   - `Protocol`: Protocol selection (`OpenPgp` or `Cms`).
   - GPG CLI / Keyring Utilities:
     - `gpg_get_executable`: Resolves configured `gpg.program` or defaults to
       `"gpg"`.
     - `gpg_nth_column`: Parses colon-separated (`--with-colons`) output lines.
     - `gpg_get_uid`: Retrieves the primary User ID for a key fingerprint.
     - `gpg_lookup_key`: Searches public keyring by query string.
     - `gpg_list_secret_keys`: Returns fingerprints of all available secret
       keys.
     - `gpg_encrypt_to_file`: Encrypts data to a file for a recipient
       fingerprint using GPG CLI.
     - `gpg_decrypt_from_file`: Decrypts a file using GPG CLI.

---

## GPG / OpenPGP Subsystem (`src/gpg.rs`)

### `Protocol`

An enum specifying the cryptographic protocol in use.

```ts
enum Protocol {
  OpenPgp = "OpenPGP",
  Cms = "CMS",
}
```

### `Data`

Represents binary or ASCII-armored data passed to and returned from
cryptographic operations.

```ts
class Data {
  static from_bytes(bytes: Uint8Array): Data;
  static from_armored_string(armored_str: string): Data;
  bytes(): Uint8Array;
  is_armored(): boolean;
  into_bytes(): Uint8Array;
}
```

### `Key`

Represents an OpenPGP public or secret key parsed via `rPGP`.

```ts
class Key {
  static from_public_bytes(bytes: Uint8Array): Key;
  static from_secret_bytes(bytes: Uint8Array): Key;
  is_secret(): boolean;
  key_id(): string;
  fingerprint(): string;
  user_ids(): string[];
  public_key(): SignedPublicKey;
}
```

### `VerificationResult`

Contains the result of verifying a digital signature.

```ts
interface VerificationResult {
  is_valid: boolean;
  signer_key_id: string | null;
  creation_time: number | null;
  message: string;
}
```

### `Context`

Executes cryptographic operations using `rPGP`.

```ts
class Context {
  constructor(protocol?: Protocol);
  set_armor(armor: boolean): Context;
  protocol(): Protocol;
  encrypt(recipients: Key[], data: Data): Data;
  decrypt(secret_key: Key, passphrase: string | null, data: Data): Data;
  sign(secret_key: Key, passphrase: string | null, data: Data): Data;
  verify(public_key: Key, signed_data: Data): VerificationResult;
}
```

### GPG CLI Helpers

Functions for interacting with the system `gpg` command and local keyrings:

```ts
function gpg_get_executable(): string;
function gpg_nth_column(line: string, col: number): string;
function gpg_get_uid(fingerprint: string): string;
function gpg_lookup_key(query: string): string[];
function gpg_list_secret_keys(): string[];
function gpg_encrypt_to_file(
  filename: string,
  recipient_fingerprint: string,
  key_is_trusted: boolean,
  input: Uint8Array,
): void;
function gpg_decrypt_from_file(filename: string): Uint8Array;
```

---

## Symmetric Encryption Subsystem (`src/crypto.rs`)

### `CryptoMode`

Enum representing repository active encryption modes:

```ts
enum CryptoMode {
  Filter = 0, // Worktree filter drivers (git-crypt compatible)
  EntireRepo = 1, // Full repository remote pack & manifest encryption (git-remote-gcrypt compatible)
  Hybrid = 2, // Layered / Default mode combining git-crypt filters and gcrypt full repo encryption
}
```

### `SymmetricCryptoKey`

ChaCha20-Poly1305 symmetric key for repository encryption.

```ts
class SymmetricCryptoKey {
  constructor(bytes?: Uint8Array);
  static generateRandom(): SymmetricCryptoKey;
  static fromBytes(bytes: Uint8Array): SymmetricCryptoKey;
  asBytes(): Buffer;
  encrypt(plaintext: Uint8Array): Buffer;
  decrypt(ciphertext: Uint8Array): Buffer;
  static isEncrypted(data: Uint8Array): boolean;
}
```

### `Crypto`

Bound to a Git repository handle to manage encrypted repository state, worktree
filters, manifest handling, and packfile encryption.

```ts
class Crypto {
  isInitialised(): boolean;
  mode(): CryptoMode;
  keyDir(): string;
  generateKey(): SymmetricCryptoKey;
  init(opts?: CryptFilterOptions): string;
  initEntireRepo(): string;
  isEntireRepoMode(): boolean;
  lock(opts?: CryptFilterOptions): void;
  unlock(key_file?: string, opts?: CryptFilterOptions): void;
  exportKey(output_path: string): void;
  importKey(input_path: string): void;
  encrypt(data: Buffer): Buffer;
  decrypt(data: Buffer): Buffer;
  isEncrypted(data: Buffer): boolean;
  encryptManifest(manifest: RepoManifest): Buffer;
  decryptManifest(data: Buffer): RepoManifest;
  encryptPack(packfile: Buffer): EncryptedPackResult;
  decryptPack(encryptedBytes: Buffer, hashType: string, packId: string, packKey: string): Buffer;
}
```

### `RepoManifest` and Manifest Utilities

```ts
interface ManifestRef {
  objectId: string;
  name: string;
}

interface ManifestPack {
  hashType: string; // e.g. "SHA256"
  packId: string;
  key: string;
}

interface ManifestKeep {
  hashType: string;
  packId: string;
  count: number;
}

interface RepoManifest {
  refs: ManifestRef[];
  packs: ManifestPack[];
  keep: ManifestKeep[];
  repoId: string;
  extensions: string[];
}

function serialiseManifest(manifest: RepoManifest): string;
function parseManifest(text: string): RepoManifest;
```

---

## Example Usage

### OpenPGP Encryption & Verification (`rPGP`)

```ts
import { Context, Data, Key, Protocol } from "@alloy-ts/repository";

const ctx = new Context(Protocol.OpenPgp);
ctx.set_armor(true);

// Parse public and secret keys
const pubKey = Key.from_public_bytes(pubKeyBytes);
const secKey = Key.from_secret_bytes(secKeyBytes);

console.log(`Key ID: ${pubKey.key_id()}, Fingerprint: ${pubKey.fingerprint()}`);

// Encrypt
const plaintext = Data.from_bytes(Buffer.from("Sensitive repository payload"));
const encryptedData = ctx.encrypt([pubKey], plaintext);

// Decrypt
const decryptedData = ctx.decrypt(secKey, "passphrase", encryptedData);
console.log(decryptedData.bytes().toString("utf-8"));

// Sign & Verify
const signedData = ctx.sign(secKey, "passphrase", plaintext);
const result = ctx.verify(pubKey, signedData);
console.log(`Signature valid: ${result.is_valid}`);
```

### GPG CLI Keyring Lookup

```ts
import { gpg_get_uid, gpg_list_secret_keys, gpg_lookup_key } from "@alloy-ts/repository";

// Search keyring for fingerprints
const fps = gpg_lookup_key("jsmith@example.com");
if (fps.length > 0) {
  const uid = gpg_get_uid(fps[0]);
  console.log(`Found key: ${fps[0]} (${uid})`);
}

// List secret keys
const secretFps = gpg_list_secret_keys();
console.log("Secret key fingerprints:", secretFps);
```
