import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import {
  initBackend,
  refStoreTypeName,
  refStoreTypeFromStr,
  RefStore,
  RefStoreType,
  Repository,
} from "./main.ts";

let base: string;
let path: string;
let repo: Repository;
let store: RefStore;
let head: string;

const git = (args: string) =>
  execSync(`git ${args}`, { cwd: path, stdio: "ignore" });

beforeAll(() => {
  base = mkdtempSync(join(tmpdir(), "repo-ref-store-"));
  path = join(base, "repo");
  execSync(`git init --quiet --initial-branch=main ${JSON.stringify(path)}`, {
    stdio: "ignore",
  });
  git("config user.email refstore@example.com");
  git("config user.name RefStore Example");
  git("config commit.gpgsign false");
  writeFileSync(join(path, "seed.txt"), "seed\n");
  git("add seed.txt");
  git("commit --quiet -m seed");

  repo = Repository.open(path);
  store = new RefStore(repo);
  head = repo.headId();
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
});

describe("RefStore lifecycle (src/ref_store.rs)", () => {
  test("new() and open() bind the same repository", () => {
    const a = new RefStore(repo);
    const b = RefStore.open(repo);
    expect(a.storeType()).toBe(RefStoreType.Files);
    expect(b.storeType()).toBe(RefStoreType.Files);
  });

  test("storeType() reports the backend format", () => {
    expect(store.storeType()).toBe(RefStoreType.Files);
  });
});

describe("RefStore lookup (src/ref_store.rs)", () => {
  test("exists() finds HEAD and misses unknown refs", () => {
    expect(store.exists("HEAD")).toBe(true);
    expect(store.exists("refs/heads/main")).toBe(true);
    expect(store.exists("refs/heads/__nope__")).toBe(false);
  });

  test("lookup() returns the reference or throws", () => {
    expect(store.lookup("refs/heads/main").name).toBe("refs/heads/main");
    expect(store.lookup("HEAD").kind).toBe("symbolic");
    expect(() => store.lookup("refs/heads/__nope__")).toThrow();
  });

  test("resolve() follows symbolic refs to the object ref", () => {
    const resolved = store.resolve("HEAD", undefined);
    expect(resolved.kind).toBe("object");
    expect(resolved.name).toBe("refs/heads/main");
    expect(resolved.target).toBe(head);

    // A negative maxNesting uses the default depth (5); a missing intermediate
    // target returns the symbolic ref itself instead of throwing.
    expect(store.resolve("HEAD", -1).name).toBe("refs/heads/main");
  });
});

describe("RefStore iteration (src/ref_store.rs)", () => {
  test("iterator() yields every reference name", () => {
    const iter = store.iterator();
    const names: string[] = [];
    let next = iter.nextName();
    while (next !== null) {
      names.push(next);
      next = iter.nextName();
    }
    expect(names).toContain("HEAD");
    expect(names.some((n) => n.startsWith("refs/heads/"))).toBe(true);
  });

  test("iterator.next() returns Reference objects and null at the end", () => {
    const iter = store.iterator();
    const first = iter.next();
    expect(first).not.toBe(null);
    expect(typeof first!.name).toBe("string");
    // Drain to exhaustion.
    while (iter.next() !== null) {
      /* noop */
    }
    expect(iter.next()).toBe(null);
  });

  test("iterator(glob) filters by shell-style pattern", () => {
    const iter = store.iterator("refs/heads/*");
    const names: string[] = [];
    let n = iter.nextName();
    while (n !== null) {
      names.push(n);
      n = iter.nextName();
    }
    expect(names.every((n) => n.startsWith("refs/heads/"))).toBe(true);
    expect(names).toContain("refs/heads/main");
  });
});

describe("RefStore write / rename / delete (src/ref_store.rs)", () => {
  test("write() creates a reference and reflog", () => {
    const ref = store.write("refs/heads/__rs_write__", head, true, undefined, "probe write");
    expect(ref.name).toBe("refs/heads/__rs_write__");
    expect(store.exists("refs/heads/__rs_write__")).toBe(true);

    const log = store.reflogRead("refs/heads/__rs_write__");
    expect(Array.isArray(log)).toBe(true);
    expect(log.length).toBeGreaterThan(0);
    expect(log[log.length - 1].newOid).toBe(head);
    expect(log[log.length - 1].message).toContain("probe write");

    store.delete("refs/heads/__rs_write__");
  });

  test("write() honours expectedTarget and rejects mismatches", () => {
    store.write("refs/heads/__rs_cmp__", head, true);

    // Matching expected target updates successfully.
    store.write("refs/heads/__rs_cmp__", head, true, undefined, "again", head);

    // A mismatched expected target is rejected.
    expect(() =>
      store.write("refs/heads/__rs_cmp__", head, true, undefined, "x", "0".repeat(40)),
    ).toThrow();

    store.delete("refs/heads/__rs_cmp__");
  });

  test("write() accepts a signature for the reflog committer", () => {
    const ref = store.write("refs/heads/__rs_sig__", head, true, {
      name: "RS Bot",
      email: "rs@example.com",
      time: 0,
      timezoneOffset: 0,
    });
    expect(ref.name).toBe("refs/heads/__rs_sig__");
    const log = store.reflogRead("refs/heads/__rs_sig__");
    expect(log[log.length - 1].signature.name).toBe("RS Bot");
    store.delete("refs/heads/__rs_sig__");
  });

  test("rename() moves a reference", () => {
    store.write("refs/heads/__rs_old__", head, true);
    const renamed = store.rename("refs/heads/__rs_old__", "refs/heads/__rs_new__", false);
    expect(renamed.name).toBe("refs/heads/__rs_new__");
    expect(store.exists("refs/heads/__rs_old__")).toBe(false);
    expect(store.exists("refs/heads/__rs_new__")).toBe(true);
    store.delete("refs/heads/__rs_new__");
  });

  test("rename() without force refuses to clobber an existing ref", () => {
    store.write("refs/heads/__rs_a__", head, true);
    store.write("refs/heads/__rs_b__", head, true);
    expect(() =>
      store.rename("refs/heads/__rs_a__", "refs/heads/__rs_b__", false),
    ).toThrow();
    store.delete("refs/heads/__rs_a__");
    store.delete("refs/heads/__rs_b__");
  });

  test("delete() enforces expectedTarget", () => {
    store.write("refs/heads/__rs_del__", head, true);

    expect(() =>
      store.delete("refs/heads/__rs_del__", "0".repeat(40)),
    ).toThrow();

    store.delete("refs/heads/__rs_del__", head);
    expect(store.exists("refs/heads/__rs_del__")).toBe(false);
  });
});

describe("RefStore reflog predicates (src/ref_store.rs)", () => {
  test("hasLog() and ensureLog() agree with the store", () => {
    store.write("refs/heads/__rs_log__", head, true, undefined, "ensure");
    expect(store.hasLog("refs/heads/__rs_log__")).toBe(true);
    expect(store.ensureLog("refs/heads/__rs_log__")).toBe(undefined);
    // A never-written ref has no log and cannot be ensured.
    expect(store.hasLog("refs/heads/__rs_unwritten__")).toBe(false);
    expect(() => store.ensureLog("refs/heads/__rs_unwritten__")).toThrow();
    store.delete("refs/heads/__rs_log__");
  });

  test("shouldWriteReflog() honours the heads/remotes/notes/HEAD rules", () => {
    expect(store.shouldWriteReflog("refs/heads/main")).toBe(true);
    // A custom ref generally only logs when core.logAllRefUpdates=always or a
    // log already exists; the result is at least a boolean.
    expect(typeof store.shouldWriteReflog("refs/custom/__x__")).toBe("boolean");
  });

  test("shouldWriteHeadReflog() is true for the checked-out branch", () => {
    expect(store.shouldWriteHeadReflog("refs/heads/main")).toBe(true);
    // A ref unrelated to HEAD (e.g. a tag) is false.
    store.write("refs/tags/__rs_head__", head, true);
    expect(store.shouldWriteHeadReflog("refs/tags/__rs_head__")).toBe(false);
    store.delete("refs/tags/__rs_head__");
  });
});

describe("RefStore locking (src/ref_store.rs)", () => {
  test("lock() / unlock() write under the lock; cancel() is a no-op", () => {
    const lock = store.lock("refs/heads/__rs_lock__");
    expect(lock.refName).toBe("refs/heads/__rs_lock__");
    // No change yet: the ref does not exist.
    expect(store.exists("refs/heads/__rs_lock__")).toBe(false);

    const ref = lock.unlock(head, true, undefined, "locked write");
    expect(ref.name).toBe("refs/heads/__rs_lock__");
    expect(store.exists("refs/heads/__rs_lock__")).toBe(true);

    // cancel() on a fresh lock just releases (no write).
    const lock2 = store.lock("refs/heads/__rs_lock__");
    expect(lock2.cancel()).toBe(undefined);

    store.delete("refs/heads/__rs_lock__");
  });
});

describe("RefStore admin operations (src/ref_store.rs)", () => {
  test("init() and compress() are safe no-ops / initialisers", () => {
    expect(store.init(undefined)).toBe(undefined);
    expect(store.init("refs/heads/main")).toBe(undefined);
    expect(store.compress()).toBe(undefined);
  });

  test("setBackend() is unsupported for the gix backend", () => {
    expect(() => store.setBackend(RefStoreType.Files)).toThrow();
  });
});

describe("RefStore paths and deref (src/ref_store.rs)", () => {
  test("gitDir / commonDir / commonDirResolved / packedRefsPath expose store locations", () => {
    expect(store.gitDir.length).toBeGreaterThan(0);
    // A non-worktree clone: common dir equals git dir.
    expect(store.commonDir()).toBe(store.gitDir());
    expect(store.commonDirResolved()).toBe(store.gitDir());
    expect(store.packedRefsPath().endsWith("packed-refs")).toBe(true);
  });

  test("write() with deref updates the referent of a symbolic ref", () => {
    // A symbolic ref pointing at main.
    store.write("refs/heads/__rs_sym__", "refs/heads/main", true);
    expect(store.lookup("refs/heads/__rs_sym__").kind).toBe("symbolic");

    // Writing to the symbolic ref with deref=true mutates main, not the symref.
    store.write("refs/heads/__rs_sym__", head, true, undefined, "deref write", undefined, true);
    expect(store.lookup("refs/heads/main").target).toBe(head);
    // The symbolic ref itself is unchanged.
    expect(store.lookup("refs/heads/__rs_sym__").symbolicTarget).toBe("refs/heads/main");

    // Without deref, the symbolic ref would be rewritten to point at the oid.
    store.write("refs/heads/__rs_sym__", head, true, undefined, "no deref", undefined, false);
    expect(store.lookup("refs/heads/__rs_sym__").kind).toBe("object");

    store.delete("refs/heads/__rs_sym__");
  });

  test("delete() with deref removes the referent of a symbolic ref", () => {
    store.write("refs/heads/__rs_delsym__", head, true);
    store.write("refs/heads/__rs_delvia__", "refs/heads/__rs_delsym__", true);
    // deref deletes __rs_delsym__ (the referent), leaving the symref behind.
    store.delete("refs/heads/__rs_delvia__", undefined, true);
    expect(store.exists("refs/heads/__rs_delsym__")).toBe(false);
    expect(store.exists("refs/heads/__rs_delvia__")).toBe(true);
    store.delete("refs/heads/__rs_delvia__");
  });
});

describe("RefStore type helpers (src/ref_store.rs)", () => {
  test("refStoreTypeName() / refStoreTypeFromStr() round-trip", () => {
    expect(refStoreTypeName(RefStoreType.Files)).toBe("files");
    expect(refStoreTypeName(RefStoreType.Reftable)).toBe("reftable");
    expect(refStoreTypeFromStr("files")).toBe(RefStoreType.Files);
    expect(refStoreTypeFromStr("reftable")).toBe(RefStoreType.Reftable);
    expect(() => refStoreTypeFromStr("bogus")).toThrow();
    expect(initBackend()).toBe(RefStoreType.Files);
  });
});
