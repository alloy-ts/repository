# Crypto & Git-Crypt API Documentation

`@alloy-ts/repository` provides symmetric key encryption and `git-crypt` path & filter configuration utilities.

## Standalone Git-Crypt Helpers

- `configureGitFilters(keyName?: string)`: Configures clean, smudge, required, and diff filter settings in git config.
- `deconfigureGitFilters(keyName?: string)`: Removes git-crypt filter entries from git config.
- `getInternalStatePath(): string`: Returns internal git-crypt path (`<gitDir>/git-crypt`).
- `getInternalKeysPath(keyName?: string): string`: Returns internal keys directory path.
- `getRepoStatePath(): string`: Returns repository state path (`<worktree>/.git-crypt`).
- `getRepoKeysPath(): string`: Returns repository keys path (`<worktree>/.git-crypt/keys`).
- `getFileAttributes(filename: string): FileAttributes`: Returns `filter` and `diff` attribute values for a file.
- `checkIfBlobIsEncrypted(objectId: string): boolean`: Checks if blob object starts with `\0GITCRYPT\0`.
- `checkIfFileIsEncrypted(filename: string): boolean`: Checks if index entry for file is encrypted.
- `getEncryptedFiles(keyName?: string): string[]`: Lists all encrypted files in index matching the specified key.

## `Crypto` & `SymmetricCryptoKey`

`Repository.crypt()` provides access to symmetric encryption bound to the open repository.
