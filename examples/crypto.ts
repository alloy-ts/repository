/**
 * Example: Demonstrating git-crypt filters, path utilities, and symmetric encryption via `@alloy-ts/repository`.
 *
 * Usage:
 *   npx tsx examples/crypto.ts
 */

import {
  Repository,
  SymmetricCryptoKey,
  configureGitFilters,
  deconfigureGitFilters,
  getInternalStatePath,
  getInternalKeysPath,
  getRepoStatePath,
  getRepoKeysPath,
  getFileAttributes,
  checkIfFileIsEncrypted,
  getEncryptedFiles,
  gpgGetExecutable,
  gpgListSecretKeys,
  GpgContext,
  GpgProtocol,
  GpgData,
} from "../src/main.ts";

function main() {
  console.log("=== Git-Crypt & GPG Utilities Example ===");

  // 1. Inspect GPG environment
  const gpgExe = gpgGetExecutable();
  console.log(`GPG Executable: ${gpgExe}`);

  const secretKeys = gpgListSecretKeys();
  console.log(`Available GPG Secret Keys count: ${secretKeys.length}`);

  // 2. Symmetric Crypto Key Operations
  console.log("\n--- Symmetric Crypto Key ---");
  const key = SymmetricCryptoKey.generateRandom();
  const plainText = Buffer.from("Sensitive confidential payload");
  const cipherText = key.encrypt(plainText);

  console.log(`Is Payload Encrypted: ${SymmetricCryptoKey.isEncrypted(cipherText)}`);
  const decrypted = key.decrypt(cipherText);
  console.log(`Decrypted Payload: "${decrypted.toString()}"`);

  // 3. Open or discover Repository
  try {
    const repo = Repository.discover();
    console.log(`\nRepository Path: ${repo.path()}`);

    // Repository Crypt Handle
    const crypt = repo.crypt();
    console.log(`Crypt Initialized: ${crypt.isInitialized()}`);

    // Git-Crypt Path Helpers
    console.log("\n--- Git-Crypt Paths ---");
    console.log(`Internal State Path: ${getInternalStatePath()}`);
    console.log(`Internal Keys Path:  ${getInternalKeysPath()}`);
    console.log(`Repo State Path:     ${getRepoStatePath()}`);
    console.log(`Repo Keys Path:      ${getRepoKeysPath()}`);

    // File Attributes & Encrypted Files Search
    console.log("\n--- File Attributes ---");
    const attrs = getFileAttributes("ReadMe.md");
    console.log(`ReadMe.md Filter: "${attrs.filter}", Diff: "${attrs.diff}"`);

    const encryptedFiles = getEncryptedFiles();
    console.log(`Encrypted Files Count: ${encryptedFiles.length}`);

    // Configure / Deconfigure Filters Example
    console.log("\n--- Filter Management ---");
    configureGitFilters();
    console.log("Configured git-crypt filters in repository config.");

    deconfigureGitFilters();
    console.log("Deconfigured git-crypt filters from repository config.");
  } catch (e) {
    console.log(`Note: Not in a git repository or discovery failed: ${(e as Error).message}`);
  }

  // 4. GPG Context Usage
  console.log("\n--- GPG Context Example ---");
  const gpgCtx = new GpgContext(GpgProtocol.OpenPgp);
  gpgCtx.setArmor(true);
  console.log(`GPG Protocol: ${gpgCtx.protocol()}, Armor Mode: ${gpgCtx.armor()}`);

  const gpgData = GpgData.fromString("Hello OpenPGP!");
  console.log(`GpgData String: "${gpgData.toString()}"`);
}

main();
