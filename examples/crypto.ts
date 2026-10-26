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

  // 2a. Default Hybrid Mode (git-crypt clean/smudge filters + git-remote-gcrypt entire repo encryption)
  const repoId = crypt.init();
  assert.strictEqual(crypt.isInitialized(), true);
  assert.strictEqual(crypt.mode(), 2); // CryptoMode.Hybrid (2)
  assert.ok(crypt.isEntireRepoMode());
  console.log("Crypt default Hybrid mode initialized with Remote Repo ID:", repoId);

  const secretData = Buffer.from("Confidential repository payload");
  const encryptedPayload = crypt.encrypt(secretData);
  assert.ok(crypt.isEncrypted(encryptedPayload));
  const decryptedPayload = crypt.decrypt(encryptedPayload);
  assert.deepEqual(decryptedPayload, secretData);

  const keyBackupPath = join(baseDir, "repo.key");
  crypt.exportKey(keyBackupPath);
  crypt.lock();
  crypt.unlock(keyBackupPath);

  console.log("Partial Mode (git-crypt filter drivers) passed.");

  // 2b. Entire Repository Encryption Mode (git-remote-gcrypt packfile & manifest encryption)
  const fullRepoId = crypt.initEntireRepo();
  assert.ok(typeof fullRepoId === "string");
  assert.ok(crypt.isEntireRepoMode());

  // When both filter and entire_repo are configured, active mode is Hybrid (2)
  assert.strictEqual(crypt.mode(), 2); // CryptoMode.Hybrid (2)
  console.log("Hybrid Mode active (git-crypt + git-remote-gcrypt full repo encryption).");

  // Manifest management in Entire Repo / Hybrid Mode
  const manifest = {
    refs: [{ objectId: "1234567890123456789012345678901234567890", name: "refs/heads/main" }],
    packs: [{ hashType: "SHA256", packId: "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2", key: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef" }],
    keep: [],
    repoId: fullRepoId,
    extensions: ["extn comment"],
  };

  const encryptedManifest = crypt.encryptManifest(manifest);
  assert.ok(crypt.isEncrypted(encryptedManifest));

  const decryptedManifest = crypt.decryptManifest(encryptedManifest);
  assert.strictEqual(decryptedManifest.refs[0].name, "refs/heads/main");

  // Packfile encryption
  const packfileBytes = Buffer.from("git object store packfile content");
  const encPack = crypt.encryptPack(packfileBytes);
  assert.strictEqual(encPack.hashType, "SHA256");

  const decPack = crypt.decryptPack(encPack.encryptedBytes, encPack.hashType, encPack.packId, encPack.packKey);
  assert.deepEqual(decPack, packfileBytes);

  console.log("Entire Repo / Hybrid Mode manifest & packfile encryption passed.");
} finally {
  rmSync(baseDir, { recursive: true, force: true });
}

// 3. OpenPGP / GPG Context & Data abstractions
const gpgCtx = new GpgContext(Protocol.OpenPgp);
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

// 4. Encrypted Repository with SSH & gcrypt Remote Configuration
const sshRepoDir = mkdtempSync(join(tmpdir(), "example-ssh-gcrypt-repo-"));

try {
  const sshRepo = Repository.init(sshRepoDir, false);
  const crypt = sshRepo.crypt();

  // Initialize Hybrid Mode (git-crypt filters + full gcrypt repo encryption)
  const remoteRepoId = crypt.init();
  assert.ok(crypt.isEntireRepoMode());

  // Configure gcrypt remote using SSH transport:
  // Format: gcrypt::git@github.com:user/project.git#encrypted
  // - gcrypt:: tells Git to use the encrypted remote helper
  // - #encrypted specifies the target encrypted branch on the remote server
  const sshRemoteUrl = "gcrypt::git@github.com:yourname/my-secret-project.git#encrypted";
  const participantFingerprint = "1234567890ABCDEF1234567890ABCDEF12345678";

  sshRepo.createRemote("origin", sshRemoteUrl);

  const localConfigPath = sshRepo.gitDir() + "/config";
  const tx = sshRepo.configFileMut(localConfigPath);
  tx.setRawValue("remote", "origin", "gcrypt-participants", participantFingerprint);
  tx.setRawValue("remote", "origin", "gcrypt-require-explicit-force-push", "true");
  tx.setRawValue("user", null, "signingkey", participantFingerprint);
  tx.commit();

  // Re-open repo to read updated config snapshot from gix
  const repoLoaded = Repository.open(sshRepoDir);
  const originRemote = repoLoaded.findRemote("origin");
  assert.strictEqual(originRemote.url, sshRemoteUrl);

  const snap = repoLoaded.configSnapshot();
  assert.strictEqual(snap.string("remote.origin.gcrypt-participants"), participantFingerprint);
  assert.strictEqual(snap.string("remote.origin.gcrypt-require-explicit-force-push"), "true");
  assert.strictEqual(snap.string("user.signingkey"), participantFingerprint);

  // Create an encrypted manifest representing remote refs and encrypted packfile states
  const loadedCrypt = repoLoaded.crypt();
  const sshManifest = {
    refs: [{ objectId: "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2", name: "refs/heads/master" }],
    packs: [],
    keep: [],
    repoId: remoteRepoId,
    extensions: ["extn comment"],
  };

  const encryptedManifest = loadedCrypt.encryptManifest(sshManifest);
  const decryptedManifest = loadedCrypt.decryptManifest(encryptedManifest);
  assert.strictEqual(decryptedManifest.refs[0].name, "refs/heads/master");
  assert.strictEqual(decryptedManifest.repoId, remoteRepoId);

  // Direct rsync-over-SSH option url format:
  // gcrypt::rsync://user@your-server.com/home/user/encrypted-repos/my-project
  const rsyncRemoteUrl = "gcrypt::rsync://user@your-server.com/home/user/encrypted-repos/my-project";
  const rsyncRemote = sshRepo.createRemote("rsync-origin", rsyncRemoteUrl);
  assert.strictEqual(rsyncRemote.url, rsyncRemoteUrl);

  console.log("Encrypted Repository with SSH & gcrypt Remote Configuration passed.");
} finally {
  rmSync(sshRepoDir, { recursive: true, force: true });
}

console.log("\nAll crypto and git-crypt example assertions completed successfully.");
