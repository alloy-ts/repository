import { test, expect, describe } from "vitest";
import { Vault, VaultStructureBinding, VaultAccessProfile, KeyManager } from "../main.ts";
import * as fs from "node:fs";
import * as path from "node:path";
import * as os from "node:os";

describe("Vault NAPI Full Parity", () => {
  test("Vault parity methods and openWithKey", () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "vault-parity-"));
    const vaultPath = path.join(tmpDir, "parity.vault");

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
        API_KEY: {
          nonce: "bm9uY2Ux",
          ciphertext: "secret_v1",
          created_at: new Date().toISOString(),
          updated_at: new Date().toISOString(),
          tags: { env: "prod" },
          history: [
            {
              nonce: "bm9uY2Uw",
              ciphertext: "secret_v0",
              updated_at: new Date().toISOString(),
            },
          ],
        },
      },
      age_recipients: [],
      wrapped_dek: null,
      version: 1,
      integrity: null,
    };

    fs.writeFileSync(vaultPath, JSON.stringify(vaultContent, null, 2));

    const vault = Vault.open(vaultPath);
    expect(vault.path()).toBe(vaultPath);
    expect(vault.schema()).toBe("tsafe/vault/v3");
    expect(vault.accessProfile()).toBe(VaultAccessProfile.ReadWrite);
    expect(vault.structureBinding()).toBe(VaultStructureBinding.Bound);
    expect(vault.rollbackSuspected()).toBe(false);
    expect(vault.isTeamVault()).toBe(false);
    expect(Vault.isTeamVault(vaultPath)).toBe(false);

    expect(vault.getVersion("API_KEY", 0)).toBe("secret_v1");
    expect(vault.getVersion("API_KEY", 1)).toBe("secret_v0");

    const history = vault.history("API_KEY");
    expect(history.length).toBe(2);
    expect(history[0].version).toBe(0);
    expect(history[1].version).toBe(1);

    const exported = vault.exportAll();
    expect(exported["API_KEY"]).toBe("secret_v1");

    vault.revertToVersion("API_KEY", 1);
    expect(vault.getVersion("API_KEY", 0)).toBe("secret_v0");

    vault.pruneHistory("API_KEY", 0);
    const prunedHistory = vault.history("API_KEY");
    expect(prunedHistory.length).toBe(1);

    vault.save();

    const dummyKey = Buffer.alloc(32);
    const vaultFromKey = Vault.openWithKey(vaultPath, dummyKey);
    expect(vaultFromKey.path()).toBe(vaultPath);

    fs.rmSync(tmpDir, { recursive: true, force: true });
  });
});
