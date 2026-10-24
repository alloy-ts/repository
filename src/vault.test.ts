import { describe, expect, test, afterAll } from "vite-plus/test";
import {
  Vault,
  type VaultAccessProfile,
  type VaultStructureBinding,
} from "./main.ts";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

function tmpVaultPath(): string {
  return path.join(
    os.tmpdir(),
    `vault-test-${Math.random().toString(36).slice(2)}.vault`,
  );
}

function pw(): Buffer {
  return Buffer.from("test-master-password");
}

const createdPaths: string[] = [];

afterAll(() => {
  for (const p of createdPaths) {
    try {
      fs.rmSync(p, { force: true });
    } catch {
      /* ignore */
    }
    try {
      fs.rmSync(`${p}.lock`, { force: true, recursive: true });
    } catch {
      /* ignore */
    }
  }
});

describe("Vault", () => {
  test("creates, sets and reads secrets", () => {
    const p = tmpVaultPath();
    createdPaths.push(p);
    const v = new Vault(p, pw());
    v.set("user", "alice");
    v.set("token", "secret-token");
    v.save();

    expect(v.path()).toBe(p);
    expect(v.secretCount()).toBe(2);
    expect(v.list().sort()).toEqual(["token", "user"]);
    expect(v.get("user")).toBe("alice");
    expect(v.get("token")).toBe("secret-token");
  });

  test("save persists an encrypted vault file to disk", () => {
    const p = tmpVaultPath();
    createdPaths.push(p);
    const v = new Vault(p, pw());
    v.set("k", "v1");
    v.save();

    expect(fs.existsSync(p)).toBe(true);
    const json = JSON.parse(fs.readFileSync(p, "utf8"));
    expect(typeof json._schema).toBe("string");
    expect(json.secrets).toHaveProperty("k");
  });

  test("delete removes a secret", () => {
    const p = tmpVaultPath();
    createdPaths.push(p);
    const v = new Vault(p, pw());
    v.set("a", "1");
    v.set("b", "2");
    v.delete("a");
    expect(v.list()).toEqual(["b"]);
    expect(() => v.get("a")).toThrow();
  });

  test("history, getVersion and revertToVersion", () => {
    const p = tmpVaultPath();
    createdPaths.push(p);
    const v = new Vault(p, pw());
    v.set("k", "v1");
    v.set("k", "v2");

    const hist = v.history("k");
    expect(hist.length).toBe(2);
    expect(hist[0].version).toBe(0);
    expect(hist[1].version).toBe(1);
    expect(typeof hist[0].timestamp).toBe("string");

    expect(v.getVersion("k", 0)).toBe("v2");
    expect(v.getVersion("k", 1)).toBe("v1");

    v.revertToVersion("k", 1);
    expect(v.get("k")).toBe("v1");
    expect(v.history("k").length).toBe(3);
  });

  test("pruneHistory keeps at most keep_n previous versions", () => {
    const p = tmpVaultPath();
    createdPaths.push(p);
    const v = new Vault(p, pw());
    v.set("k", "v1");
    v.set("k", "v2");
    v.set("k", "v3");
    v.pruneHistory("k", 1);
    // current (version 0) + at most 1 history entry.
    expect(v.history("k").length).toBeLessThanOrEqual(2);
  });

  test("exportAll returns every secret", () => {
    const p = tmpVaultPath();
    createdPaths.push(p);
    const v = new Vault(p, pw());
    v.set("a", "1");
    v.set("b", "2");
    expect(v.exportAll()).toEqual({ a: "1", b: "2" });
  });

  test("access profile, structure binding and rollback flag", () => {
    const p = tmpVaultPath();
    createdPaths.push(p);
    const v = new Vault(p, pw());
    v.set("x", "y");
    expect(v.accessProfile()).toBe<VaultAccessProfile>("ReadWrite");
    expect(v.structureBinding()).toBe<VaultStructureBinding>("Bound");
    expect(v.rollbackSuspected()).toBe(false);
  });

  test("parseRotationDays parses duration policies", () => {
    expect(Vault.parseRotationDays("90d")).toBe(90);
    expect(Vault.parseRotationDays("garbage")).toBe(null);
  });

  test("isTeamVault is false for a local vault", () => {
    const p = tmpVaultPath();
    createdPaths.push(p);
    new Vault(p, pw());
    expect(Vault.isTeamVault(p)).toBe(false);
  });

  test("openWithKey rejects a non-32-byte key", () => {
    const p = tmpVaultPath();
    createdPaths.push(p);
    new Vault(p, pw());
    expect(() => Vault.openWithKey(p, Buffer.alloc(16))).toThrow();
  });

  test("rotate changes the master password", () => {
    const p = tmpVaultPath();
    createdPaths.push(p);
    {
      const v = new Vault(p, pw());
      v.set("k", "value");
      v.rotate(Buffer.from("new-password"));
    }
    // Release the first handle so its lock is freed before reopening.
    globalThis.gc?.();
    // The on-disk vault is now encrypted under the new password.
    expect(() => Vault.open(p, pw())).toThrow();
    const reopened = Vault.open(p, Buffer.from("new-password"));
    expect(reopened.get("k")).toBe("value");
  });

  test("reopen reads persisted data from disk", () => {
    const p = tmpVaultPath();
    createdPaths.push(p);
    {
      const v = new Vault(p, pw());
      v.set("k", "v1");
      v.save();
    }
    // Allow the first handle's lock to be released.
    globalThis.gc?.();
    const v = Vault.open(p, pw());
    expect(v.get("k")).toBe("v1");
  });

  test("createWithAccessProfile creates a read-only vault", () => {
    const p = tmpVaultPath();
    createdPaths.push(p);
    const v = Vault.createWithAccessProfile(p, pw(), "ReadOnly");
    expect(v.accessProfile()).toBe("ReadOnly");
    expect(() => v.set("k", "v")).toThrow();
  });

  test("liveLockHolder and lockState report advisory lock details", () => {
    const p = tmpVaultPath();
    createdPaths.push(p);
    const v = new Vault(p, pw());
    expect(Vault.liveLockHolder(p)).toBe(process.pid);
    const state = Vault.lockState(p);
    expect(state).not.toBeNull();
    expect(state?.state).toBe("HeldBy");
    expect(state?.pid).toBe(process.pid);
  });

  test("validateSecretKey validates key names", () => {
    expect(() => Vault.validateSecretKey("VALID_KEY")).not.toThrow();
    expect(() => Vault.validateSecretKey("app/DB_PASS")).not.toThrow();
    expect(() => Vault.validateSecretKey("")).toThrow();
    expect(() => Vault.validateSecretKey("café")).toThrow();
  });

  test("isSupportedSchema and schemaIsBound check vault formats", () => {
    expect(Vault.isSupportedSchema("tsafe/vault/v3")).toBe(true);
    expect(Vault.isSupportedSchema("tsafe/vault/v1")).toBe(true);
    expect(Vault.isSupportedSchema("invalid/schema")).toBe(false);

    expect(Vault.schemaIsBound("tsafe/vault/v3")).toBe(true);
    expect(Vault.schemaIsBound("tsafe/vault/v1")).toBe(false);
  });
});
