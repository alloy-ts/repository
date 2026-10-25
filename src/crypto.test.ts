import { describe, expect, test, beforeAll, afterAll } from "vite-plus/test";
import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  Repository,
  Crypto,
  SymmetricCryptoKey,
  configureGitFilters,
  deconfigureGitFilters,
  getInternalStatePath,
  getInternalKeysPath,
  getRepoStatePath,
  getRepoKeysPath,
  getFileAttributes,
  checkIfBlobIsEncrypted,
  checkIfFileIsEncrypted,
  getEncryptedFiles,
  gpgGetExecutable,
  gpgListSecretKeys,
  GpgData,
  GpgContext,
  GpgProtocol,
} from "./main.ts";

let dir: string;
let repo: Repository;

beforeAll(() => {
  dir = mkdtempSync(join(tmpdir(), "repo-crypto-test-"));
  execSync("git init -q -b main", { cwd: dir, stdio: "ignore" });
  writeFileSync(join(dir, ".gitattributes"), "secret.txt filter=git-crypt diff=git-crypt\n");
  writeFileSync(join(dir, "secret.txt"), "sensitive content\n");
  execSync("git -c user.email=t@t.t -c user.name=t add -A", { cwd: dir, stdio: "ignore" });
  execSync("git -c user.email=t@t.t -c user.name=t -c commit.gpgsign=false commit -q -m init", { cwd: dir, stdio: "ignore" });
  repo = Repository.open(dir);
});

afterAll(() => {
  rmSync(dir, { recursive: true, force: true });
});

describe("Crypto class & SymmetricCryptoKey via NAPI", () => {
  test("generates random key and encrypts/decrypts buffer", () => {
    const key = SymmetricCryptoKey.generateRandom();
    expect(key).toBeDefined();

    const plain = Buffer.from("top secret message");
    const encrypted = key.encrypt(plain);
    expect(encrypted.length).toBeGreaterThan(plain.length);
    expect(SymmetricCryptoKey.isEncrypted(encrypted)).toBe(true);

    const decrypted = key.decrypt(encrypted);
    expect(decrypted.toString()).toBe("top secret message");
  });

  test("Repository.crypt() handle initialization, lock & unlock", () => {
    const crypt = repo.crypt();
    expect(crypt.isInitialized()).toBe(false);

    crypt.init();
    expect(crypt.isInitialized()).toBe(true);
    expect(crypt.keyDir()).toContain("crypt");

    const plain = Buffer.from("crypto handle text");
    const cipher = crypt.encrypt(plain);
    expect(crypt.isEncrypted(cipher)).toBe(true);
    expect(crypt.decrypt(cipher).toString()).toBe("crypto handle text");

    crypt.lock();
  });
});

describe("Git-Crypt Standalone Helpers", () => {
  test("configureGitFilters and deconfigureGitFilters", () => {
    const origCwd = process.cwd();
    process.chdir(dir);
    try {
      expect(() => configureGitFilters()).not.toThrow();
      expect(() => configureGitFilters("customKey")).not.toThrow();
      expect(() => deconfigureGitFilters()).not.toThrow();
      expect(() => deconfigureGitFilters("customKey")).not.toThrow();
    } finally {
      process.chdir(origCwd);
    }
  });

  test("path getters return proper git-crypt paths", () => {
    const origCwd = process.cwd();
    process.chdir(dir);
    try {
      const statePath = getInternalStatePath();
      expect(statePath).toContain("git-crypt");

      const keysPath = getInternalKeysPath();
      expect(keysPath).toContain("keys");

      const namedKeysPath = getInternalKeysPath("defaultKey");
      expect(namedKeysPath).toContain("defaultKey");

      const repoStatePath = getRepoStatePath();
      expect(repoStatePath).toContain(".git-crypt");

      const repoKeysPath = getRepoKeysPath();
      expect(repoKeysPath).toContain("keys");
    } finally {
      process.chdir(origCwd);
    }
  });

  test("getFileAttributes and getEncryptedFiles", () => {
    const origCwd = process.cwd();
    process.chdir(dir);
    try {
      const attrs = getFileAttributes("secret.txt");
      expect(attrs.filter).toBeDefined();
      expect(attrs.filter.length).toBeGreaterThan(0);
      expect(attrs.diff).toBeDefined();
      expect(attrs.diff.length).toBeGreaterThan(0);

      const encryptedFiles = getEncryptedFiles();
      expect(Array.isArray(encryptedFiles)).toBe(true);
      expect(encryptedFiles).toContain("secret.txt");
    } finally {
      process.chdir(origCwd);
    }
  });

  test("checkIfFileIsEncrypted and checkIfBlobIsEncrypted", () => {
    const origCwd = process.cwd();
    process.chdir(dir);
    try {
      const isEncrypted = checkIfFileIsEncrypted("secret.txt");
      expect(isEncrypted).toBe(false); // Plain text in index initially
    } finally {
      process.chdir(origCwd);
    }
  });
});

describe("GPG NAPI Bindings & Helpers", () => {
  test("gpgGetExecutable returns configured or default binary name", () => {
    const exe = gpgGetExecutable();
    expect(typeof exe).toBe("string");
    expect(exe.length).toBeGreaterThan(0);
  });

  test("gpgListSecretKeys returns an array", () => {
    const keys = gpgListSecretKeys();
    expect(Array.isArray(keys)).toBe(true);
  });

  test("GpgData buffers and string conversions", () => {
    const data = GpgData.fromString("gpg text payload");
    expect(data.toString()).toBe("gpg text payload");

    const bufData = GpgData.fromBuffer(Buffer.from("buffer payload"));
    expect(bufData.toBuffer().toString()).toBe("buffer payload");
  });

  test("GpgContext creation and protocol check", () => {
    const ctx = new GpgContext(GpgProtocol.OpenPgp);
    expect(ctx.protocol()).toBe(GpgProtocol.OpenPgp);

    ctx.setArmor(true);
    expect(ctx.armor()).toBe(true);

    ctx.setOffline(true);
    expect(ctx.offline()).toBe(true);
  });
});
