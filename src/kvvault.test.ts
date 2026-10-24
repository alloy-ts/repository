import { describe, expect, test, afterAll } from "vite-plus/test";
import { KvVault } from "./main.ts";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

function tmpKvPath(): string {
  return path.join(
    os.tmpdir(),
    `kvvault-test-${Math.random().toString(36).slice(2)}`,
  );
}

const createdPaths: string[] = [];

afterAll(() => {
  for (const p of createdPaths) {
    try {
      fs.rmSync(p, { force: true, recursive: true });
    } catch {
      /* ignore */
    }
  }
});

describe("KvVault", () => {
  test("stores, retrieves, lists, and deletes key-value pairs", () => {
    const p = tmpKvPath();
    createdPaths.push(p);

    const kv = new KvVault(p);
    kv.put("config/app", '{"name":"myapp"}');
    kv.put("config/db", '{"host":"localhost"}');
    kv.put("secrets/token", "opaque-secret-payload");

    expect(kv.get("config/app")).toBe('{"name":"myapp"}');
    expect(kv.get("config/db")).toBe('{"host":"localhost"}');
    expect(kv.get("nonexistent")).toBeNull();

    const configKeys = kv.list("config/");
    expect(configKeys).toEqual(["config/app", "config/db"]);

    const allKeys = kv.list("");
    expect(allKeys.sort()).toEqual(["config/app", "config/db", "secrets/token"]);

    kv.delete("config/app");
    expect(kv.get("config/app")).toBeNull();
    expect(kv.list("config/")).toEqual(["config/db"]);
  });
});
