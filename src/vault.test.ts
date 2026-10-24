import { describe, expect, test } from "vite-plus/test";
import { Vault, VaultStructureBinding, VaultAccessProfile } from "./main.ts";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const PASSWORD = Buffer.from("test-password-123");

function withTempDir(fn: (dir: string) => void) {
  const dir = mkdtempSync(join(tmpdir(), "vault-test-"));
  try {
    fn(dir);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

describe("Vault NAPI extensions", () => {
  test("create, set, get, save, exportAll, and properties", () => {
    withTempDir((dir) => {
      const path = join(dir, "my.vault");
      const vault = new Vault(path, PASSWORD);

      expect(vault.path).toBe(path);
      expect(vault.accessProfile).toBe(VaultAccessProfile.ReadWrite);
      expect(vault.structureBinding).toBe(VaultStructureBinding.Bound);
      expect(vault.rollbackSuspected).toBe(false);

      vault.set("DB_PASS", "s3cr3t", { rotate_policy: "30d" });
      vault.set("API_KEY", "xyz123");

      expect(vault.get("DB_PASS")).toBe("s3cr3t");
      expect(vault.secretCount()).toBe(2);
      expect(vault.list()).toEqual(["API_KEY", "DB_PASS"]);

      const exported = vault.exportAll();
      expect(exported).toEqual({
        DB_PASS: "s3cr3t",
        API_KEY: "xyz123",
      });

      vault.save();

      // Reopen vault
      const reopened = Vault.open(path, PASSWORD);
      expect(reopened.get("DB_PASS")).toBe("s3cr3t");
      expect(reopened.get("API_KEY")).toBe("xyz123");
    });
  });

  test("version history, revertToVersion, and pruneHistory", () => {
    withTempDir((dir) => {
      const path = join(dir, "history.vault");
      const vault = new Vault(path, PASSWORD);

      vault.set("TOKEN", "v1");
      vault.set("TOKEN", "v2");
      vault.set("TOKEN", "v3");

      expect(vault.get("TOKEN")).toBe("v3");

      const hist = vault.history("TOKEN");
      expect(hist.length).toBe(3); // current (v0) + 2 history items
      expect(hist[0].version).toBe(0);

      expect(vault.getVersion("TOKEN", 0)).toBe("v3");
      expect(vault.getVersion("TOKEN", 1)).toBe("v2");
      expect(vault.getVersion("TOKEN", 2)).toBe("v1");

      vault.revertToVersion("TOKEN", 1);
      expect(vault.get("TOKEN")).toBe("v2");

      vault.pruneHistory("TOKEN", 1);
      expect(vault.history("TOKEN").length).toBe(2); // current (v0) + 1 pruned history item
    });
  });

  test("isTeamVault static helper", () => {
    withTempDir((dir) => {
      const path = join(dir, "personal.vault");
      new Vault(path, PASSWORD);
      expect(Vault.isTeamVault(path)).toBe(false);
    });
  });
});
