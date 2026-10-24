import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Vault } from "./main.ts";

let base: string;
let vaultPath: string;

beforeAll(() => {
  base = mkdtempSync(join(tmpdir(), "vault-test-"));
  vaultPath = join(base, "secrets.vault");
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true });
});

describe("Vault NAPI bindings (src/vault.rs)", () => {
  test("status reports Absent when vault file does not exist", () => {
    const status = Vault.status(vaultPath);
    expect(status.exists).toBe(false);
    expect(status.homeState).toBe("Absent");
  });

  test("Vault.create creates a new vault file", () => {
    const vault = Vault.create(vaultPath, "masterpass");
    expect(vault.path()).toBe(vaultPath);

    const status = Vault.status(vaultPath);
    expect(status.exists).toBe(true);
    expect(status.homeState).toBe("Initialized");
  });

  test("Vault.open opens an existing vault with password check", () => {
    expect(() => Vault.open(vaultPath, "")).toThrow("invalid vault password");
    const vault = Vault.open(vaultPath, "masterpass");
    expect(vault.path()).toBe(vaultPath);
    expect(vault.list()).toEqual([]);
  });

  test("set, get, list, exportAll, and delete manage secrets", () => {
    const vault = Vault.open(vaultPath, "masterpass");

    vault.set("DB_PASSWORD", "s3cr3t");
    vault.set("API_KEY", "12345");

    expect(vault.get("DB_PASSWORD")).toBe("s3cr3t");
    expect(vault.get("API_KEY")).toBe("12345");
    expect(vault.get("MISSING")).toBe(null);

    expect(vault.list()).toEqual(["API_KEY", "DB_PASSWORD"]);

    const exported = vault.exportAll();
    expect(exported.DB_PASSWORD).toBe("s3cr3t");
    expect(exported.API_KEY).toBe("12345");

    const deleted = vault.delete("API_KEY");
    expect(deleted).toBe(true);
    expect(vault.get("API_KEY")).toBe(null);
    expect(vault.list()).toEqual(["DB_PASSWORD"]);
  });

  test("renameKey renames keys", () => {
    const vault = Vault.open(vaultPath, "masterpass");
    vault.renameKey("DB_PASSWORD", "PROD_DB_PASSWORD");
    expect(vault.get("DB_PASSWORD")).toBe(null);
    expect(vault.get("PROD_DB_PASSWORD")).toBe("s3cr3t");
  });

  test("history and revertToVersion manage versioning", () => {
    const vault = Vault.open(vaultPath, "masterpass");
    const history = vault.history("PROD_DB_PASSWORD");
    expect(history.length).toBeGreaterThanOrEqual(1);

    expect(() => vault.revertToVersion("PROD_DB_PASSWORD", 0)).not.toThrow();
  });

  test("rotate updates master password", () => {
    let vault = Vault.open(vaultPath, "masterpass");
    vault.rotate("newmasterpass");

    vault = Vault.open(vaultPath, "newmasterpass");
    expect(vault.get("PROD_DB_PASSWORD")).toBe("s3cr3t");
  });

  test("snapshot returns vault overview", () => {
    const vault = Vault.open(vaultPath, "newmasterpass");
    const snap = vault.snapshot();
    expect(snap.formatVersion).toBe(3);
    expect(snap.legacySecrets.length).toBe(1);
    expect(snap.legacySecrets[0].name).toBe("PROD_DB_PASSWORD");
  });

  test("isTeamVault detects age recipients", () => {
    expect(Vault.isTeamVault(vaultPath)).toBe(false);
  });
});
