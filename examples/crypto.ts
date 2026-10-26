import assert from "node:assert";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  SymmetricCryptoKey,
  Repository,
  Protocol,
  GpgData,
  GpgContext,
  gpgGetExecutable,
  gpgNthColumn,
} from "../src/main.ts";

// 1. SymmetricCryptoKey: ChaCha20-Poly1305 symmetric encryption
const key = SymmetricCryptoKey.generateRandom();
assert.equal(key.asBytes().length, 32);

const plaintext = Buffer.from("Secret file content for git-crypt");
const ciphertext = key.encrypt(plaintext);

assert.ok(SymmetricCryptoKey.isEncrypted(ciphertext));
assert.strictEqual(SymmetricCryptoKey.isEncrypted(plaintext), false);

const decrypted = key.decrypt(ciphertext);
assert.deepEqual(decrypted, plaintext);

console.log("SymmetricCryptoKey encryption and decryption roundtrip passed.");

// 2. Repository-bound Crypto handle (git-crypt operations)
const baseDir = mkdtempSync(join(tmpdir(), "example-crypto-repo-"));

try {
  const repo = Repository.init(baseDir, false);
  const crypt = repo.crypt();

  assert.strictEqual(crypt.isInitialized(), false);

  // Initialize crypt in repository with git-remote-gcrypt submodule layering
  crypt.init({
    filterName: "git-crypt",
    gcryptUrl: "gcrypt::https://git.example.com/vault.git",
    submodulePath: "secure-submodule",
  });
  assert.strictEqual(crypt.isInitialized(), true);
  console.log("Crypt initialized at key directory:", crypt.keyDir());

  // Encrypt and decrypt using repository symmetric key
  const secretData = Buffer.from("Confidential repository payload");
  const encryptedPayload = crypt.encrypt(secretData);
  assert.ok(crypt.isEncrypted(encryptedPayload));

  const decryptedPayload = crypt.decrypt(encryptedPayload);
  assert.deepEqual(decryptedPayload, secretData);

  // Export repository key
  const keyBackupPath = join(baseDir, "repo.key");
  crypt.exportKey(keyBackupPath);

  // Lock and unlock repository
  crypt.lock();
  crypt.unlock(keyBackupPath);

  console.log("Repository crypt operations (init, encrypt, decrypt, export, lock, unlock) passed.");
} finally {
  rmSync(baseDir, { recursive: true, force: true });
}

// 3. OpenPGP / GPG Context & Data abstractions
const gpgCtx = new GpgContext(Protocol.OpenPgp ?? 0);
assert.strictEqual(gpgCtx.protocol, Protocol.OpenPgp ?? 0);

gpgCtx.setArmor(true);

const data = GpgData.fromBytes(Buffer.from("Payload for OpenPGP processing"));
assert.strictEqual(data.isArmored, false);
assert.deepEqual(data.bytes(), Buffer.from("Payload for OpenPGP processing"));

const armoredData = GpgData.fromArmoredString("-----BEGIN PGP MESSAGE-----\n\nhello\n-----END PGP MESSAGE-----");
assert.strictEqual(armoredData.isArmored, true);

// GPG CLI utilities
const gpgExe = gpgGetExecutable();
assert.ok(typeof gpgExe === "string");

const sampleLine = "uid:u::::1395975462::AB97D6E3E5D8789988CA55E5F77D9E7397D05229::John Smith <jsmith@example.com>:";
const uidCol = gpgNthColumn(sampleLine, 9);
assert.strictEqual(uidCol, "John Smith <jsmith@example.com>");

console.log("GPG / OpenPGP context, data, and CLI helper assertions passed.");
console.log("\nAll crypto and git-crypt example assertions completed successfully.");
