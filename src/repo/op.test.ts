import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, expect, it } from "vite-plus/test";

import {
  Repository,
  Workspace,
  WorkspaceLoader,
  defaultBackendFactories,
  defaultIndexStoreInitializer,
  defaultOpHeadsStoreInitializer,
  defaultOpStoreInitializer,
  defaultSubmoduleStoreInitializer,
  defaultWorkingCopyFactories,
  defaultWorkspaceStoreInitializer,
  getCurrentHeadOps,
  reparentRange,
  resolveOpAt,
  resolveOpForLoad,
  resolveOpWithRepo,
  SimpleBackend,
  SqlBackend,
  OpStore,
  COMMIT_ID_LENGTH,
  CHANGE_ID_LENGTH,
  Operation,
  ViewId,
  OperationId,
  RemoteRef,
  type RemoteRefState,
  View,
  Store,
  COMMIT_CACHE_CAPACITY,
  TREE_CACHE_CAPACITY,
} from "../../dist/index.js";

describe("Repository Op and Workspace NAPI bindings", () => {
  it("initializes an op repo with default opsDir <git-dir>/ops and queries head operations and op resolution", () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "repo-op-test-"));
    const repo = Repository.init(tmpDir);

    expect(repo.isOpRepo()).toBe(false);
    const headsStore = repo.initOpRepo();
    expect(repo.isOpRepo()).toBe(true);

    const opsPath = path.join(tmpDir, ".git", "ops");
    expect(fs.existsSync(opsPath)).toBe(true);

    const headOps = repo.currentHeadOps();
    expect(headOps.length).toBeGreaterThan(0);
    const rootOp = headOps[0]!;

    const resolved = repo.resolveOp("@");
    expect(resolved.id).toBe(rootOp.id);

    const resolvedLoad = resolveOpForLoad(repo, "@");
    expect(resolvedLoad.id).toBe(rootOp.id);

    const resolvedWithRepo = resolveOpWithRepo(repo, "@");
    expect(resolvedWithRepo.id).toBe(rootOp.id);

    const opStore = repo.OpStore();
    const resolvedAt = resolveOpAt(opStore, headOps, "@");
    expect(resolvedAt.id).toBe(rootOp.id);

    const directHeads = getCurrentHeadOps(headsStore, opStore);
    expect(directHeads.length).toBe(headOps.length);

    fs.rmSync(tmpDir, { recursive: true, force: true });
  });

  it("supports core.dbUrl and auto-initializes opsDir and store.db even without running init", () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "repo-dburl-test-"));
    // Standard git init without storeOp
    const repo = Repository.init(tmpDir);
    expect(repo.isOpRepo()).toBe(false);

    // Set core.dbUrl in config
    const customDb = path.join(tmpDir, "custom.db");
    repo.configSet("core.dbUrl", `file:${customDb}`);

    // Reopen repository: isOpRepo auto-detects and initializes opsDir and store.db
    const reopened = Repository.open(tmpDir);
    expect(reopened.dbUrl()).toBe(`file:${customDb}`);
    expect(reopened.isOpRepo()).toBe(true);
    expect(fs.existsSync(customDb)).toBe(true);

    fs.rmSync(tmpDir, { recursive: true, force: true });
  });

  it("tests OpStoreOpts with custom opsDir and recursive creation", () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "opstore-opts-test-"));
    const nestedOpsDir = path.join(tmpDir, "nested", "deep", "ops_store");

    const store = new OpStore({ opsDir: nestedOpsDir, create: true });
    expect(store).toBeDefined();
    expect(fs.existsSync(nestedOpsDir)).toBe(true);

    const opened = OpStore.open({ opsDir: nestedOpsDir, create: true });
    expect(opened).toBeDefined();

    fs.rmSync(tmpDir, { recursive: true, force: true });
  });

  it("tests SqlBackend with SQLite WAL mode", () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "sql-backend-test-"));
    const dbPath = path.join(tmpDir, "ops.sqlite");

    const sqlBackend = SqlBackend.init(dbPath);
    expect(sqlBackend.name()).toBe("SqlBackend");
    expect(sqlBackend.isWalMode()).toBe(true);

    const fId = sqlBackend.writeFile("sql backend file content");
    expect(sqlBackend.readFile(fId)).toBe("sql backend file content");

    const tId = sqlBackend.writeTree("sql tree content");
    expect(sqlBackend.readTree(tId)).toBe("sql tree content");

    const cId = sqlBackend.writeCommit("sql commit content");
    expect(sqlBackend.readCommit(cId)).toBe("sql commit content");

    fs.rmSync(tmpDir, { recursive: true, force: true });
  });

  it("performs op reparenting", () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "repo-reparent-test-"));
    const repo = Repository.init(tmpDir);
    repo.initOpRepo();
    const opStore = repo.OpStore();

    const rootOp = repo.resolveOp("@");

    // Write a child operation
    const childId = opStore.writeOperation({
      id: "",
      parents: [rootOp.id],
      viewId: rootOp.viewId,
      metadata: {
        ...rootOp.metadata,
        description: "child op",
      },
    });

    const childOp = opStore.readOperation(childId);

    // Write a destination op
    const destId = opStore.writeOperation({
      id: "",
      parents: [],
      viewId: rootOp.viewId,
      metadata: {
        ...rootOp.metadata,
        description: "new base",
      },
    });
    const destOp = opStore.readOperation(destId);

    const reparentStats = reparentRange(opStore, [rootOp], [childOp], destOp);
    expect(reparentStats.rewrittenCount).toBe(1);
    expect(reparentStats.newHeadIds.length).toBe(1);

    const rewrittenHead = opStore.readOperation(reparentStats.newHeadIds[0]!);
    expect(rewrittenHead.parents).toContain(destId);

    fs.rmSync(tmpDir, { recursive: true, force: true });
  });

  it("handles Workspace init_simple, loader, mutation lock, and checkout", () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "repo-workspace-test-"));
    const ws = Workspace.initSimple(tmpDir, "test-ws");

    expect(ws.workspaceRoot()).toBe(tmpDir);
    expect(ws.workspaceName()).toBe("test-ws");

    const loader = new WorkspaceLoader(tmpDir);
    expect(loader.workspaceRoot()).toBe(tmpDir);
    expect(loader.getWorkingCopyType()).toBe("local");

    const loadedWs = loader.load();
    expect(loadedWs.workspaceName()).toBe("test-ws");

    // Lock and finish
    const locked = ws.startWorkingCopyMutation();
    locked.finish("op_xyz");

    // Check out
    const stats = ws.checkOut("op_abc", null);
    expect(stats.updatedFiles).toBeGreaterThanOrEqual(1);

    fs.rmSync(tmpDir, { recursive: true, force: true });
  });

  it("provides factory and initializer utility functions", () => {
    const factories = defaultBackendFactories();
    expect(factories.supportedBackends()).toContain("git");
    expect(factories.supportedOpStores()).toContain("simple");

    const wcFactories = defaultWorkingCopyFactories();
    expect(wcFactories["local"]).toBe("LocalWorkingCopyFactory");

    expect(defaultWorkspaceStoreInitializer()).toBe("simple");
    expect(defaultOpStoreInitializer()).toBe("sql");
    expect(defaultOpHeadsStoreInitializer()).toBe("simple");
    expect(defaultIndexStoreInitializer()).toBe("default");
    expect(defaultSubmoduleStoreInitializer()).toBe("default");
  });

  it("tests SimpleBackend storage and constants", () => {
    expect(COMMIT_ID_LENGTH).toBe(64);
    expect(CHANGE_ID_LENGTH).toBe(16);

    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "simple-backend-test-"));
    const backend = SimpleBackend.init(tmpDir);

    expect(backend.name()).toBe("Simple");
    expect(backend.commitIdLength()).toBe(64);
    expect(backend.changeIdLength()).toBe(16);
    expect(backend.concurrency()).toBe(1);
    expect(backend.rootCommitId().length).toBe(128);
    expect(backend.rootChangeId().length).toBe(32);
    expect(typeof backend.emptyTreeId()).toBe("string");

    // File write and read
    const fileHash = backend.writeFile("hello simple backend");
    expect(fileHash.length).toBe(40);
    expect(backend.readFile(fileHash)).toBe("hello simple backend");

    // Symlink write and read
    const symlinkHash = backend.writeSymlink("target/path");
    expect(backend.readSymlink(symlinkHash)).toBe("target/path");

    // Tree write and read
    const treeHash = backend.writeTree("dummy tree proto");
    expect(backend.readTree(treeHash)).toBe("dummy tree proto");

    // Commit write and read
    const commitHash = backend.writeCommit("dummy commit proto");
    expect(backend.readCommit(commitHash)).toBe("dummy commit proto");
    expect(backend.readCommit(backend.rootCommitId())).toBe("root_commit");

    fs.rmSync(tmpDir, { recursive: true, force: true });
  });

  it("supports InitOptions opsDir and init.opsDir custom configuration", () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "repo-custom-ops-"));
    const customOpsDir = path.join(tmpDir, "custom-ops");

    const repo = Repository.initOpts(tmpDir, {
      storeOp: true,
      opsDir: customOpsDir,
    });

    expect(repo.isOpRepo()).toBe(true);
    expect(fs.existsSync(path.join(customOpsDir, "op_store"))).toBe(true);
    expect(fs.existsSync(path.join(customOpsDir, "op_heads"))).toBe(true);

    fs.rmSync(tmpDir, { recursive: true, force: true });
  });

  it("tests Store caching wrapper over backend", () => {
    expect(COMMIT_CACHE_CAPACITY).toBe(100);
    expect(TREE_CACHE_CAPACITY).toBe(1000);

    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "store-test-"));
    const store = Store.init(tmpDir);

    expect(store.commitIdLength()).toBe(64);
    expect(store.changeIdLength()).toBe(16);

    const fId = store.writeFile("cached content");
    expect(store.readFile(fId)).toBe("cached content");

    const tId = store.writeTree("tree content");
    expect(store.getTree(tId)).toBe("tree content");

    const cId = store.writeCommit("commit content");
    expect(store.getCommit(cId)).toBe("commit content");

    store.clearCaches();
    expect(store.getCommit(cId)).toBe("commit content");

    fs.rmSync(tmpDir, { recursive: true, force: true });
  });

  it("tests Op, Op.fromCommits, OperationId, ViewId, RemoteRef, and View", () => {
    const viewId = new ViewId("deadbeef");
    expect(viewId.hex()).toBe("deadbeef");

    const opId = new OperationId("cafebabe");
    expect(opId.hex()).toBe("cafebabe");

    const absentRef = RemoteRef.absent();
    expect(absentRef.isAbsent()).toBe(true);
    expect(absentRef.isPresent()).toBe(false);

    const normalRef = new RemoteRef({ commitId: "123456" }, 1 as RemoteRefState);
    expect(normalRef.isAbsent()).toBe(false);
    expect(normalRef.isPresent()).toBe(true);
    expect(normalRef.isTracked()).toBe(true);

    const view = View.makeRoot("000000");
    expect(view.headIds()).toEqual(["000000"]);
    view.setLocalBookmark("main", { commitId: "123456" });
    expect(view.getLocalBookmark("main")?.commitId).toBe("123456");

    // Test Operation class directly
    const op1 = new Operation("op-1", "view-1", [], "initial op", { branch: "main" });
    expect(op1.id().hex()).toBe("op-1");
    expect(op1.viewId().hex()).toBe("view-1");
    expect(op1.parents()).toEqual([]);
    expect(op1.description()).toBe("initial op");
    expect(op1.metadata().attributes["branch"]).toBe("main");

    const record = op1.toRecord();
    expect(record.id).toBe("op-1");
    expect(record.viewId).toBe("view-1");

    // Test compose operations
    const op2 = new Operation("op-2", "view-2", ["op-1"], "second op", { author: "alice" });
    const composed = Operation.compose([op1, op2]);
    expect(composed.parentIds()).toEqual([]);
    expect(composed.viewId().hex()).toBe("view-2");
    expect(composed.description()).toContain("initial op");
    expect(composed.description()).toContain("second op");
    expect(composed.metadata().attributes["branch"]).toBe("main");
    expect(composed.metadata().attributes["author"]).toBe("alice");

    // Test Operation.from_commit with real Git commits
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "op-from-commits-"));
    const repo = Repository.init(tmpDir);
    const emptyTreeId = repo.writeTree([]);
    const c1 = repo.commit("refs/heads/main", "Commit 1", emptyTreeId, []);
    const c2 = repo.commit("refs/heads/main", "Commit 2", emptyTreeId, [c1]);

    const commitObj1 = repo.findCommit(c1);
    const commitObj2 = repo.findCommit(c2);

    const singleOp1 = Operation.from_commit(repo, commitObj1);
    expect(singleOp1.description()).toBe("Commit 1");
    expect(singleOp1.parentIds()).toEqual([]);

    const singleOp2 = Operation.from_commit(repo, commitObj2, [singleOp1.id().hex()]);
    expect(singleOp2.description()).toBe("Commit 2");
    expect(singleOp2.parentIds().map((p) => p.hex())).toEqual([singleOp1.id().hex()]);

    // Convert commit dag to operation dag using from_commit
    const commitIds = [c1, c2];
    const dagOps: Operation[] = [];
    const commitToOp = new Map<string, string>();
    for (const cid of commitIds) {
      const c = repo.findCommit(cid);
      const parentOpIds = c.parentIds.map((pid) => commitToOp.get(pid)!);
      const op = Operation.from_commit(repo, c, parentOpIds);
      commitToOp.set(cid, op.id().hex());
      dagOps.push(op);
    }

    expect(dagOps.length).toBe(2);
    expect(dagOps[0]!.description()).toBe("Commit 1");
    expect(dagOps[1]!.description()).toBe("Commit 2");
    expect(dagOps[1]!.parentIds().map((p) => p.hex())).toContain(dagOps[0]!.id().hex());

    // Test methods on Operation
    expect(singleOp1.storesCommitPredecessors()).toBe(true);
    expect(Array.isArray(singleOp1.allReferencedCommitIds())).toBe(true);
    expect(singleOp1.storeOperation().id).toBe(singleOp1.id().hex());

    fs.rmSync(tmpDir, { recursive: true, force: true });
  });
});
