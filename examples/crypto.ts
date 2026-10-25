import assert from "node:assert";
import * as fs from "node:fs";
import * as path from "node:path";
import * as os from "node:os";
import { Repository, SymmetricCryptoKey, GpgManager } from "../src/main.ts";

const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "git-crypt-example-"));

try {
  // 1. Initialize a repository and access the crypt subsystem
  const repo = Repository.init(tmpDir, false);
  const crypt = repo.crypt();

  assert.equal(crypt.isInitialized(), false);

  // 2. Initialize crypt filters & default symmetric key
  crypt.init();
  assert.equal(crypt.isInitialized(), true);
  console.log("Crypt initialized at key dir:", crypt.keyDir());

  // 3. Encrypt & Decrypt payloads
  const plaintext = Buffer.from("Sensitive source file content");
  const ciphertext = crypt.encrypt(plaintext);

  assert.equal(crypt.isEncrypted(ciphertext), true);
  assert.equal(SymmetricCryptoKey.isEncrypted(ciphertext), true);
  console.log("Encrypted payload size:", ciphertext.length, "bytes");

  const decrypted = crypt.decrypt(ciphertext);
  assert.deepEqual(decrypted, plaintext);
  console.log("Decrypted payload:", decrypted.toString());

  // 4. Export & Import symmetric repository key
  const exportPath = path.join(tmpDir, "exported-repo.key");
  crypt.exportKey(exportPath);
  assert.ok(fs.existsSync(exportPath));
  console.log("Exported key to:", exportPath);

  // Generate a new key and then re-import the saved key
  const freshKey = crypt.generateKey();
  assert.equal(freshKey.asBytes().length, 32);

  crypt.importKey(exportPath);
  assert.deepEqual(crypt.decrypt(ciphertext), plaintext);

  // 5. Lock and Unlock repository
  crypt.lock();
  crypt.unlock();

  // 6. Demonstrate SymmetricCryptoKey CSPRNG key generation
  const randomKey = SymmetricCryptoKey.generateRandom();
  assert.equal(randomKey.asBytes().length, 32);

  const standaloneEnc = randomKey.encrypt(Buffer.from("standalone crypt payload"));
  const standaloneDec = randomKey.decrypt(standaloneEnc);
  assert.deepEqual(standaloneDec, Buffer.from("standalone crypt payload"));

  console.log("\nAll git-crypt example assertions passed successfully.");
} finally {
  fs.rmSync(tmpDir, { recursive: true, force: true });
}
