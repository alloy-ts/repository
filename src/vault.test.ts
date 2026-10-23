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

  test("Vault.open opens an existing vault", () => {
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

  test("isTeamVault detects age recipients", () => {
    expect(Vault.isTeamVault(vaultPath)).toBe(false);
  });
});
