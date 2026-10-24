import { test, expect, describe } from "vitest";
import { KvVault } from "../main.ts";
import * as fs from "node:fs";
import * as path from "node:path";
import * as os from "node:os";

describe("KvVault Fjall Store", () => {
  test("put, get, list, delete operations", () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "kvvault-test-"));
    const storePath = path.join(tmpDir, "kvstore");

    const kv = KvVault.open(storePath);

    expect(kv.get("key1")).toBeNull();

    kv.put("app/db_pass", "secret123");
    kv.put("app/api_token", "token456");
    kv.put("other/setting", "value");

    expect(kv.get("app/db_pass")).toBe("secret123");
    expect(kv.get("app/api_token")).toBe("token456");

    const appKeys = kv.list("app/");
    expect(appKeys).toEqual(["app/api_token", "app/db_pass"]);

    kv.delete("app/db_pass");
    expect(kv.get("app/db_pass")).toBeNull();

    fs.rmSync(tmpDir, { recursive: true, force: true });
  });
});
