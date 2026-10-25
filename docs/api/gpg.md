# GPG API Documentation

The `gpg` module provides N-API bindings for GPG operations using `gpgme`.

## Classes & Objects

### `GpgContext`

Manages GPG contexts and operations.

#### Methods

- `constructor(protocol?: GpgProtocol)`: Initialize GPG context with specified protocol (defaults to `OpenPgp`).
- `protocol(): GpgProtocol`: Get current protocol.
- `setArmor(armor: boolean)` / `armor(): boolean`: Control ASCII armor output.
- `setTextmode(textmode: boolean)` / `textmode(): boolean`: Control canonical text mode.
- `setOffline(offline: boolean)` / `offline(): boolean`: Control offline mode (disables network lookups like CRL/OCSP/WKD).
- `findKey(pattern: string): GpgKey | null`: Find key by pattern (keyid, fingerprint, email, user id).
- `findKeys(patterns: string[]): GpgKey[]`: Search public keys.
- `findSecretKeys(patterns: string[]): GpgKey[]`: Search secret keys.
- `addSigner(keyPattern: string)` / `clearSigners()`: Manage signers for signing operations.
- `encrypt(recipients: string[], plaintext: GpgData, ciphertext: GpgData)`: Encrypt data for recipients.
- `encryptSymmetric(plaintext: GpgData, ciphertext: GpgData)`: Encrypt data symmetrically with passphrase.
- `decrypt(ciphertext: GpgData, plaintext: GpgData)`: Decrypt data.
- `sign(mode: GpgSignMode, plaintext: GpgData, signature: GpgData)`: Sign data.
- `verify(signature: GpgData, signedText?: GpgData, outputPlain?: GpgData): GpgVerificationResult`: Verify signature.
- `importKeys(keyData: GpgData): number`: Import keys and return number of imported keys.
- `exportKeys(pattern: string | null, outputData: GpgData)`: Export keys.
- `createKey(userid: string, algo?: string)`: Create new keypair.

### `GpgData`

Container for data passed to cryptographic operations.

#### Methods

- `constructor()`: Create empty buffer.
- `GpgData.fromBuffer(buf: Buffer)`: Create from Buffer.
- `GpgData.fromString(text: string)`: Create from string.
- `GpgData.fromFile(path: string)`: Create referencing file path.
- `toBuffer(): Buffer`: Read data to Buffer.
- `toString(): string`: Read data to string.

## Standalone Helpers

- `gpgGetExecutable(): string`: Get configured `gpg.program` executable path.
- `gpgGetUid(fingerprint: string): string`: Get primary user ID (name and email) for fingerprint.
- `gpgLookupKey(query: string): string[]`: Lookup matching key fingerprints.
- `gpgListSecretKeys(): string[]`: List all secret key fingerprints in keyring.
- `gpgEncryptToFile(filename: string, recipientFingerprint: string, keyIsTrusted: bool, data: Buffer)`: Encrypt buffer to file.
- `gpgDecryptFromFile(filename: string): Buffer`: Decrypt file content into buffer.
