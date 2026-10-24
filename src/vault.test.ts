import { test, expect, describe } from "vitest";
import { KeyManager, IdentityManager, Vault, StructureBinding } from "../main.ts";
import * as fs from "node:fs";
import * as path from "node:path";
import * as os from "node:os";

describe("Vault & KeyManager & IdentityManager bindings", () => {
  test("KeyManager initialization and paths", () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "km-test-"));
    const km = new KeyManager(tmpDir);
    expect(km.isInitialized()).toBe(false);
    expect(km.gitCryptDir()).toContain("crypt");
    expect(km.defaultKeyPath()).toContain("default");

    km.initDirs();
    expect(km.isInitialized()).toBe(true);
    fs.rmSync(tmpDir, { recursive: true, force: true });
  });

  test("Vault status and open on a mock vault file", () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "vault-test-"));
    const vaultPath = path.join(tmpDir, "test.vault");

    const vaultContent = {
      _schema: "tsafe/vault/v3",
      kdf: {
        algorithm: "argon2id",
        m_cost: 65536,
        t_cost: 3,
        p_cost: 4,
        salt: "c2FsdHNhbHRzYWx0",
      },
      cipher: "XChaCha20Poly1305",
      vault_challenge: {
        nonce: "bm9uY2Vub25jZQ==",
        ciphertext: "Y2hhbGxlbmdlY3Q=",
      },
      created_at: new Date().toISOString(),
      updated_at: new Date().toISOString(),
      secrets: {
        DB_PASSWORD: {
          nonce: "bm9uY2Ux",
          ciphertext: "Y2lwaGVyMQ==",
          created_at: new Date().toISOString(),
          updated_at: new Date().toISOString(),
          tags: { env: "prod" },
          history: [],
        },
      },
      age_recipients: [],
      wrapped_dek: null,
      version: 1,
      integrity: null,
    };

    fs.writeFileSync(vaultPath, JSON.stringify(vaultContent, null, 2));

    const status = Vault.status(vaultPath);
    expect(status.exists).toBe(true);
    expect(status.formatVersion).toBe(3);
    expect(status.secretCount).toBe(1);
    expect(status.isTeamVault).toBe(false);

    const vault = Vault.open(vaultPath);
    expect(vault.path()).toBe(vaultPath);
    expect(vault.schema()).toBe("tsafe/vault/v3");
    expect(vault.secretCount()).toBe(1);
    expect(vault.list()).toEqual(["DB_PASSWORD"]);

    const entry = vault.getSecretEntry("DB_PASSWORD");
    expect(entry).not.toBeNull();
    expect(entry?.tags.env).toBe("prod");

    fs.rmSync(tmpDir, { recursive: true, force: true });
  });
});
