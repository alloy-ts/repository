# Cryptography & GPG API

The `@alloy-ts/repository` cryptography API provides GPG/OpenPGP operations, symmetric encryption, SSH key management, and stateful signatures.

## GPG Subsystem (`src/gpg.rs`)

Backed by `rPGP` (`pgp` crate), the GPG subsystem exposes standard OpenPGP structures and helper functions:

### Types

- **`Protocol`**: Protocol selection (`OpenPGP` or `Unknown`).
- **`Data`**: Payload container for `Bytes(Vec<u8>)` or `Text(String)`.
- **`Key`**: OpenPGP key inspection (`Public` or `Secret`). Methods:
  - `Key::from_public_bytes(bytes)`
  - `key.key_id()`
  - `key.fingerprint()`
  - `key.is_secret()`
- **`VerificationResult`**: Signature verification output (`is_valid`, `signer_key_id`, `signer_fingerprint`, `detail`).
- **`Context`**: Cryptographic execution context for `encrypt`, `decrypt`, `sign`, and `verify`.

### Helper Functions

- `gpg_get_executable()`: Returns the configured `gpg.program` or defaults to `"gpg"`.
- `gpg_nth_column(line, col)`: Extracts colon-separated column fields from GPG output.
- `gpg_get_uid(fingerprint)`: Retrieves user ID associated with a key fingerprint.
- `gpg_lookup_key(query)`: Searches public key fingerprints matching a search query.
- `gpg_list_secret_keys()`: Lists secret key fingerprints.
- `gpg_encrypt_to_file(filename, recipient_fingerprint, key_is_trusted, data)`: Encrypts data to a file for a recipient.
- `gpg_decrypt_from_file(filename)`: Decrypts data from a file.

## Symmetric Crypto & Vault (`src/crypto.rs`)

- **`SymmetricCryptoKey`**: ChaCha20-Poly1305 256-bit symmetric encryption/decryption (`encrypt`, `decrypt`, `is_encrypted`).
- **`GpgManager`**: Encrypts symmetric repository keys for GPG recipients.
- **Git Filter Helpers**: Configuration and management of `git-crypt` filters (`configure_git_filters_named`, `deconfigure_git_filters_named`, `git_checkout`, `check_if_file_is_encrypted`).

## Whole-Repository Encryption (`Gcrypt`)

Full-repository encryption mode compatible with `git-remote-gcrypt`:

- **`Gcrypt.formatManifest(repoId, refs, packKeys)`**: Formats a raw manifest text block containing encrypted ref mapping and packfile key entries.
- **`Gcrypt.parseManifest(content)`**: Parses raw manifest text into a `GcryptManifest` object (`repoId`, `refs`, `packKeys`, `keepList`, `extensions`).
- **`Gcrypt.generatePackKey()`**: Generates random 63-byte packfile key material.
