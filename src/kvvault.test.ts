import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { KvVault } from "./main.ts";

let base: string;
let kvPath: string;

beforeAll(() => {
  base = mkdtempSync(join(tmpdir(), "kvvault-test-"));
  kvPath = join(base, "kv.store");
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true });
});

describe("KvVault NAPI bindings (src/vault.rs)", () => {
  test("KvVault.open initializes store", () => {
    const store = KvVault.open(kvPath);
    expect(store).not.toBeNull();
  });

  test("put, get, delete, and list manage key-value pairs", () => {
    const store = KvVault.open(kvPath);

    store.put("app/db_pass", "secret123");
    store.put("app/api_token", "tok456");
    store.put("sys/host", "localhost");

    expect(store.get("app/db_pass")).toBe("secret123");
    expect(store.get("app/api_token")).toBe("tok456");
    expect(store.get("missing")).toBe(null);

    const appKeys = store.list("app/");
    expect(appKeys).toEqual(["app/api_token", "app/db_pass"]);

    const deleted = store.delete("app/api_token");
    expect(deleted).toBe(true);
    expect(store.get("app/api_token")).toBe(null);
    expect(store.list("app/")).toEqual(["app/db_pass"]);
  });
});
