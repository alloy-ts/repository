import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { mkdtempSync, rmSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Vault, KvVault, VaultAccessProfile, VaultStructureBinding } from "./main.js";

describe("Vault NAPI bindings", () => {
  let tempDir: string;
  let vaultPath: string;

  beforeEach(() => {
    tempDir = mkdtempSync(join(tmpdir(), "vault-test-"));
    vaultPath = join(tempDir, "test.vault");
  });

  afterEach(() => {
    rmSync(tempDir, { recursive: true, force: true });
  });

  test("creates, sets, gets, deletes, lists, and saves a vault", () => {
    const password = Buffer.from("super-secret-password");
    const vault = new Vault(vaultPath, password);

    vault.set("DB_USER", "postgres", new Map([["env", "prod"]]));
    vault.set("DB_PASS", "secret123");

    expect(vault.get("DB_USER")).toBe("postgres");
    expect(vault.get("DB_PASS")).toBe("secret123");
    expect(vault.list()).toEqual(["DB_PASS", "DB_USER"]);
    expect(vault.secretCount()).toBe(2);
    expect(vault.accessProfile()).toBe(VaultAccessProfile.ReadWrite);
    expect(vault.structureBinding()).toBe(VaultStructureBinding.Bound);
    expect(vault.rollbackSuspected()).toBe(false);

    vault.save();
    expect(existsSync(vaultPath)).toBe(true);

    const reopened = Vault.open(vaultPath, password);
    expect(reopened.get("DB_USER")).toBe("postgres");
    expect(reopened.get("DB_PASS")).toBe("secret123");

    reopened.delete("DB_PASS");
    reopened.save();

    const reopened2 = Vault.open(vaultPath, password);
    expect(reopened2.list()).toEqual(["DB_USER"]);
  });

  test("versioning, history, revert, and prune", () => {
    const password = Buffer.from("password");
    const vault = new Vault(vaultPath, password);

    vault.set("API_KEY", "v1");
    vault.set("API_KEY", "v2");
    vault.set("API_KEY", "v3");

    expect(vault.get("API_KEY")).toBe("v3");
    expect(vault.getVersion("API_KEY", 0)).toBe("v3");
    expect(vault.getVersion("API_KEY", 1)).toBe("v2");
    expect(vault.getVersion("API_KEY", 2)).toBe("v1");

    const history = vault.history("API_KEY");
    expect(history.length).toBe(3);
    expect(history[0].version).toBe(0);

    vault.revertToVersion("API_KEY", 2);
    expect(vault.get("API_KEY")).toBe("v1");

    vault.pruneHistory("API_KEY", 1);
    vault.save();
  });

  test("exportAll and openWithKey", () => {
    const password = Buffer.from("pass");
    const vault = new Vault(vaultPath, password);
    vault.set("K1", "V1");
    vault.set("K2", "V2");
    vault.save();

    const exported = vault.exportAll();
    expect(exported).toEqual({ K1: "V1", K2: "V2" });
  });

  test("static helpers and team vault check", () => {
    expect(Vault.isTeamVault(vaultPath)).toBe(false);
    expect(Vault.parseRotationDays("90d")).toBe(90n);
  });
});

describe("KvVault NAPI bindings", () => {
  let tempDir: string;

  beforeEach(() => {
    tempDir = mkdtempSync(join(tmpdir(), "kvvault-test-"));
  });

  afterEach(() => {
    rmSync(tempDir, { recursive: true, force: true });
  });

  test("put, get, delete, list in KvVault", () => {
    const kv = new KvVault(tempDir);

    kv.put("config/a", "val-a");
    kv.put("config/b", "val-b");
    kv.put("users/u1", "user-1");

    expect(kv.get("config/a")).toBe("val-a");
    expect(kv.get("config/b")).toBe("val-b");
    expect(kv.get("missing")).toBeNull();

    expect(kv.list("config/")).toEqual(["config/a", "config/b"]);
    expect(kv.list("")).toEqual(["config/a", "config/b", "users/u1"]);

    kv.delete("config/a");
    expect(kv.get("config/a")).toBeNull();
    expect(kv.list("config/")).toEqual(["config/b"]);
  });
});
