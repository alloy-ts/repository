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
  COMMIT_ID_LENGTH,
  CHANGE_ID_LENGTH,
} from "../../dist/index.js";

describe("Repository Op and Workspace NAPI bindings", () => {
  it("initializes an op repo and queries head operations and op resolution", () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "repo-op-test-"));
    const repo = Repository.init(tmpDir);

    expect(repo.isOpRepo()).toBe(false);
    const headsStore = repo.initOpRepo();
    expect(repo.isOpRepo()).toBe(true);

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
    expect(defaultOpStoreInitializer()).toBe("simple");
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
});
