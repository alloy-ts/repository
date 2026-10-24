import { describe, expect, test, afterAll } from "vite-plus/test";
import { KvVault } from "./main.ts";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

function tmpDbDir(): string {
  return path.join(
    os.tmpdir(),
    `kvvault-test-${Math.random().toString(36).slice(2)}`,
  );
}

const dbDirs: string[] = [];

afterAll(() => {
  for (const d of dbDirs) {
    try {
      fs.rmSync(d, { force: true, recursive: true });
    } catch {
      /* ignore */
    }
  }
});

describe("KvVault", () => {
  test("creates store, puts, gets and lists keys", () => {
    const dir = tmpDbDir();
    dbDirs.push(dir);
    const store = new KvVault(dir);

    store.put("config/a", "value-a");
    store.put("config/b", "value-b");
    store.put("users/alice", "alice-data");

    expect(store.get("config/a")).toBe("value-a");
    expect(store.get("config/b")).toBe("value-b");
    expect(store.get("users/alice")).toBe("alice-data");
    expect(store.get("nonexistent")).toBe(null);

    expect(store.list("config/")).toEqual(["config/a", "config/b"]);
    expect(store.list("").sort()).toEqual([
      "config/a",
      "config/b",
      "users/alice",
    ]);
  });

  test("deletes keys", () => {
    const dir = tmpDbDir();
    dbDirs.push(dir);
    const store = new KvVault(dir);

    store.put("key1", "val1");
    store.put("key2", "val2");
    expect(store.get("key1")).toBe("val1");

    store.delete("key1");
    expect(store.get("key1")).toBe(null);
    expect(store.list("")).toEqual(["key2"]);
  });
});
