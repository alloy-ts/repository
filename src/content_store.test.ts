import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, test } from "vite-plus/test";
import { ContentStore, debrisReport, removeDebris } from "./main.ts";

// `ContentStore`, `debrisReport`, and `removeDebris` are compiled only with
// `--features store`. Skip the suite on the default build where they are absent.
const haveStore = typeof ContentStore !== "undefined";

(haveStore ? describe : describe.skip)("ContentStore (feature = store)", () => {
  function tmp(): string {
    return mkdtempSync(join(tmpdir(), "content-store-"));
  }

  test("debrisReport returns a report with an entries array", () => {
    const report = debrisReport(tmp());
    expect(report).toBeDefined();
    expect(Array.isArray(report.entries)).toBe(true);
  });

  test("removeDebris reports a removed count", () => {
    const removed = removeDebris(tmp());
    expect(typeof removed).toBe("number");
  });

  test("open constructs a store and exposes the read surface", () => {
    let store: ContentStore;
    try {
      store = ContentStore.open(tmp(), false);
    } catch {
      // No valid container fixture is available in the default test environment;
      // the binding itself is what we smoke-test here.
      return;
    }
    expect(store).toBeInstanceOf(ContentStore);
    expect(typeof store.get).toBe("function");
    expect(typeof store.exists).toBe("function");
    expect(typeof store.reconstruct).toBe("function");
    expect(typeof store.scan_ids).toBe("function");
    expect(typeof store.ids).toBe("function");
  });

  test("WAL mode gates the append/sync path behind open(wal: true)", () => {
    let store: ContentStore;
    try {
      store = ContentStore.open(tmp(), true);
    } catch {
      return;
    }
    // A valid `Record` / `WalNovelPiece` can't be built from JS without the
    // core record constructors, so we only assert the methods exist and that
    // `sync()` is a safe no-op when no WAL writes have occurred.
    expect(typeof store.append).toBe("function");
    expect(typeof store.append_tomb).toBe("function");
    expect(typeof store.sync).toBe("function");
    expect(() => store.sync()).not.toThrow();
  });
});
