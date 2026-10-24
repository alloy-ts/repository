import { describe, expect, test } from "vite-plus/test";
import { KvVault } from "./main.ts";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

function withTempDir(fn: (dir: string) => void) {
  const dir = mkdtempSync(join(tmpdir(), "kvvault-test-"));
  try {
    fn(dir);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

describe("KvVault NAPI bindings", () => {
  test("put, get, delete, and list with prefix", () => {
    withTempDir((dir) => {
      const kv = KvVault.open(dir);

      expect(kv.get("key1")).toBeNull();

      kv.put("app/config/db", "postgres://localhost/db");
      kv.put("app/config/port", "5432");
      kv.put("user/session/1", "token123");

      expect(kv.get("app/config/db")).toBe("postgres://localhost/db");
      expect(kv.get("app/config/port")).toBe("5432");

      const appKeys = kv.list("app/");
      expect(appKeys).toEqual(["app/config/db", "app/config/port"]);

      const allKeys = kv.list();
      expect(allKeys).toEqual(["app/config/db", "app/config/port", "user/session/1"]);

      kv.delete("app/config/port");
      expect(kv.get("app/config/port")).toBeNull();
      expect(kv.list("app/")).toEqual(["app/config/db"]);
    });
  });
});
