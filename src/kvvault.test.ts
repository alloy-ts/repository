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
  test("put and get round-trips values", () => {
    const p = tmpKvPath();
    createdPaths.push(p);
    const kv = new KvVault(p);
    kv.put("a", "1");
    kv.put("b", "2");
    expect(kv.get("a")).toBe("1");
    expect(kv.get("b")).toBe("2");
    expect(kv.get("missing")).toBe(null);
  });

  test("delete removes a key", () => {
    const p = tmpKvPath();
    createdPaths.push(p);
    const kv = new KvVault(p);
    kv.put("a", "1");
    kv.put("b", "2");
    kv.delete("a");
    expect(kv.get("a")).toBe(null);
    expect(kv.get("b")).toBe("2");
  });

  test("list returns keys sharing a prefix, sorted", () => {
    const p = tmpKvPath();
    createdPaths.push(p);
    const kv = new KvVault(p);
    kv.put("user:1", "a");
    kv.put("user:2", "b");
    kv.put("team:1", "c");
    expect(kv.list("").sort()).toEqual(["team:1", "user:1", "user:2"]);
    expect(kv.list("user:").sort()).toEqual(["user:1", "user:2"]);
    expect(kv.list("team:").sort()).toEqual(["team:1"]);
    expect(kv.list("nope:")).toEqual([]);
  });

  test("reopen persists values across handles", () => {
    const p = tmpKvPath();
    createdPaths.push(p);
    {
      const kv = new KvVault(p);
      kv.put("a", "1");
      kv.put("b", "2");
    }
    globalThis.gc?.();
    const kv = new KvVault(p);
    expect(kv.get("a")).toBe("1");
    expect(kv.get("b")).toBe("2");
    expect(kv.list("").sort()).toEqual(["a", "b"]);
  });
});
