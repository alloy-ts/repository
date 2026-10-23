import { describe, expect, test } from "vitest";
import { Vault, CredentialCipher, CredentialSecrets, KvEngine } from "./main.js";

describe("Vault & Secret Store NAPI Bindings", () => {
  describe("Vault", () => {
    test("resolve, status, and root", () => {
      const v = Vault.resolve("/tmp/test-vault-home");
      expect(v.root()).toBe("/tmp/test-vault-home");
      expect(v.exists()).toBe(false);

      const status = v.status();
      expect(status.root).toBe("/tmp/test-vault-home");
      expect(status.homeState).toBe("Uninitialized");
      expect(status.exists).toBe(false);
      expect(status.formatVersion).toBeUndefined();
      expect(status.pendingTransaction).toBe(false);
    });

    test("init, setSecret, list, removeSecret", () => {
      const v = Vault.resolve("/tmp/test-vault-01");
      expect(() => v.list("pass123")).toThrow();

      v.init("pass123");
      expect(v.exists()).toBe(true);
      expect(v.status().homeState).toBe("Initialized");

      v.setSecret("pass123", "db_password", "supersecret");
      v.setSecret("pass123", "api_key", Buffer.from("key12345"));

      const secrets = v.list("pass123");
      expect(secrets.length).toBe(2);
      const dbSec = secrets.find((s) => s.name === "db_password");
      expect(dbSec).toBeDefined();
      expect(dbSec?.valueLen).toBe(11);

      const removed = v.removeSecret("pass123", "db_password");
      expect(removed).toBe(true);
      expect(v.list("pass123").length).toBe(1);
    });

    test("setField, listFields, and snapshot", () => {
      const v = Vault.resolve("/tmp/test-vault-02");
      v.init("pass123");

      v.setField("pass123", "jig://AWS/access_key", "text", "AKIAIOSFODNN7EXAMPLE");
      v.setField("pass123", "jig://AWS/secret_key", "concealed", Buffer.from("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"));

      const fields = v.listFields("pass123");
      expect(fields.length).toBe(2);
      expect(fields[0].reference).toBe("jig://AWS/access_key");
      expect(fields[0].kind).toBe("text");

      const snap = v.snapshot("pass123");
      expect(snap.vaultId).toBe("vault-01");
      expect(snap.formatVersion).toBe(3);
      expect(snap.fields.length).toBe(2);

      const removed = v.removeField("pass123", "jig://AWS/access_key");
      expect(removed).toBe(true);
      expect(v.listFields("pass123").length).toBe(1);
    });
  });

  describe("CredentialCipher & CredentialSecrets", () => {
    test("CredentialSecrets insert, get, fieldNames, len, is_empty", () => {
      const secrets = new CredentialSecrets();
      expect(secrets.isEmpty()).toBe(true);
      expect(secrets.len).toBe(0);

      secrets.insert("access_token", "token123");
      secrets.insert("refresh_token", Buffer.from("ref456"));

      expect(secrets.isEmpty()).toBe(false);
      expect(secrets.len).toBe(2);
      expect(secrets.fieldNames()).toEqual(["access_token", "refresh_token"]);

      const val = secrets.get("access_token");
      expect(val).not.toBeNull();
      expect(Buffer.from(val!).toString()).toBe("token123");
    });

    test("CredentialCipher parse, encrypt and decrypt", () => {
      const keyring = "1:SGVsbG8gV29ybGQgMTIzNDU2Nzg5MDEyMzQ1Njc4OTA=,2:QW5vdGhlciBLZXkgMTIzNDU2Nzg5MDEyMzQ1Njc4OTA=";
      const cipher = CredentialCipher.parse(keyring);

      expect(cipher.activeVersion).toBe(2);
      expect(cipher.knownVersions()).toEqual([1, 2]);

      const secrets = new CredentialSecrets();
      secrets.insert("api_key", "sk_test_123456");

      const encrypted = cipher.encrypt("scope-abc", secrets);
      expect(encrypted.scopeId).toBe("scope-abc");
      expect(encrypted.keyVersion).toBe(2);
      expect(encrypted.fields.length).toBe(1);
      expect(encrypted.fields[0].name).toBe("api_key");

      const decrypted = cipher.decrypt(encrypted);
      expect(decrypted.fieldNames()).toEqual(["api_key"]);
      expect(Buffer.from(decrypted.get("api_key")!).toString()).toBe("sk_test_123456");
    });
  });

  describe("KvEngine", () => {
    test("write, read, metadata, list and delete", () => {
      const kv = new KvEngine("secret");
      expect(kv.mount).toBe("secret");

      const info = kv.write("myapp/config", [
        { key: "username", value: "admin" },
        { key: "password", value: "secret123" },
      ]);
      expect(info.version).toBe(1);
      expect(info.destroyed).toBe(false);

      const sec = kv.read("myapp/config");
      expect(sec).not.toBeNull();
      expect(sec?.version).toBe(1);
      expect(sec?.values).toEqual([
        { key: "username", value: "admin" },
        { key: "password", value: "secret123" },
      ]);

      const keys = kv.list("myapp");
      expect(keys).toEqual(["myapp/config"]);

      const meta = kv.metadata("myapp/config");
      expect(meta).not.toBeNull();
      expect(meta?.currentVersion).toBe(1);
      expect(meta?.versions.length).toBe(1);

      const deleted = kv.delete("myapp/config");
      expect(deleted).toBe(true);

      const secAfterDelete = kv.read("myapp/config");
      expect(secAfterDelete?.deletionTime).not.toBeNull();
    });
  });
});
