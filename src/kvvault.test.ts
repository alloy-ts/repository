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
  test("put, get, list, delete operations", () => {
    const p = tmpKvPath();
    createdPaths.push(p);
    const kv = new KvVault(p);

    kv.put("app/secret", "v1");
    kv.put("app/config", "v2");
    kv.put("other/data", "v3");

    expect(kv.get("app/secret")).toBe("v1");
    expect(kv.get("app/config")).toBe("v2");
    expect(kv.get("nonexistent")).toBe(null);

    expect(kv.list("app/")).toEqual(["app/config", "app/secret"]);
    expect(kv.list("")).toEqual(["app/config", "app/secret", "other/data"]);

    kv.delete("app/config");
    expect(kv.get("app/config")).toBe(null);
    expect(kv.list("app/")).toEqual(["app/secret"]);
  });
});
