import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { OpHeadsStore, Repository } from "../main.ts";

describe("OpHeadsStore & Jujutsu (jj) Repository integration", () => {
  let tmpRoot: string;
  let jjRepoDir: string;
  let store: OpHeadsStore;

  beforeAll(() => {
    tmpRoot = mkdtempSync(join(tmpdir(), "jj-test-"));
    jjRepoDir = join(tmpRoot, "repo");
    store = OpHeadsStore.init(jjRepoDir, "1111111111111111111111111111111111111111");
  });

  afterAll(() => {
    rmSync(tmpRoot, { recursive: true, force: true });
  });

  test("init creates simple_op_heads_store and simple_op_store layout with initial root head", () => {
    expect(store.name).toBe("simple_op_heads_store");
    expect(store.path).toBe(jjRepoDir);

    const heads = store.getOpHeads();
    expect(heads.length).toBe(1);
    const head0 = heads[0]!;
    expect(head0).toBe("0".repeat(64));

    const rootOp = store.readOperation(head0);
    expect(rootOp.id).toBe("0".repeat(64));
    expect(rootOp.viewId).toBe("0".repeat(64));
    expect(rootOp.metadata.description).toBe("root operation");

    const rootView = store.readView(rootOp.viewId);
    expect(rootView.headIds).toEqual(["1111111111111111111111111111111111111111"]);
    expect(rootView.wcCommitIds["default"]).toBe("1111111111111111111111111111111111111111");
  });

  test("addOpHead and removeOpHead manipulate current op heads", () => {
    const op2 = "2".repeat(64);
    store.addOpHead(op2);

    let heads = store.getOpHeads();
    expect(heads).toContain(op2);
    expect(heads.length).toBe(2);

    store.removeOpHead(op2);
    heads = store.getOpHeads();
    expect(heads).not.toContain(op2);
    expect(heads.length).toBe(1);
  });

  test("updateOpHeads replaces old heads with a new head", () => {
    const rootOp = "0".repeat(64);
    const opA = "a".repeat(64);
    store.updateOpHeads([rootOp], opA);

    let heads = store.getOpHeads();
    expect(heads).toEqual([opA]);

    // Restore rootOp
    store.updateOpHeads([opA], rootOp);
    heads = store.getOpHeads();
    expect(heads).toEqual([rootOp]);
  });

  test("lock acquires and releases lock on op-heads-store", () => {
    const lock = store.lock();
    expect(lock).toBeDefined();
    lock.release();
  });

  test("writeOperation and readOperation round-trip metadata and parent graph", () => {
    const opId = "b".repeat(64);
    const viewId = "v".repeat(64);

    const createdId = store.writeOperation({
      id: opId,
      viewId,
      parents: ["0".repeat(64)],
      metadata: {
        description: "transaction: commit change",
        hostname: "test-box",
        username: "developer",
        isSnapshot: false,
        workspaceName: "default",
        time: {
          startMillis: 1000n,
          endMillis: 1500n,
          tzOffset: 0,
        },
        attributes: {
          branch: "main",
        },
      },
    });

    expect(createdId).toBe(opId);
    const fetched = store.readOperation(opId);
    expect(fetched.id).toBe(opId);
    expect(fetched.viewId).toBe(viewId);
    expect(fetched.parents).toEqual(["0".repeat(64)]);
    expect(fetched.metadata.description).toBe("transaction: commit change");
    expect(fetched.metadata.hostname).toBe("test-box");
    expect(fetched.metadata.attributes["branch"]).toBe("main");
  });

  test("writeView and readView round-trip repository view snapshots", () => {
    const viewId = "v1".padEnd(64, "0");
    const createdId = store.writeView({
      id: viewId,
      headIds: ["c1", "c2"],
      wcCommitIds: { default: "c2" },
      localBookmarks: {
        main: {
          adds: ["c2"],
          removes: [],
          isConflict: false,
        },
      },
      localTags: {},
      gitRefs: {},
      gitHeads: { default: { adds: ["c1"], removes: [], isConflict: false } },
    });

    expect(createdId).toBe(viewId);
    const view = store.readView(viewId);
    expect(view.headIds).toEqual(["c1", "c2"]);
    expect(view.wcCommitIds["default"]).toBe("c2");
    expect(view.localBookmarks["main"]?.adds).toEqual(["c2"]);
  });

  test("resolveOpHeads reconciles divergent operation heads into a unified merge operation", () => {
    const testDir = join(tmpRoot, "divergent-repo");
    const divStore = OpHeadsStore.init(testDir, "rootcommit");

    const v1 = divStore.writeView({
      id: "v1".padEnd(64, "0"),
      headIds: ["commitA"],
      wcCommitIds: { default: "commitA" },
      localBookmarks: { feature1: { adds: ["commitA"], removes: [], isConflict: false } },
      localTags: {},
      gitRefs: {},
      gitHeads: {},
    });

    const v2 = divStore.writeView({
      id: "v2".padEnd(64, "0"),
      headIds: ["commitB"],
      wcCommitIds: { default: "commitB" },
      localBookmarks: { feature2: { adds: ["commitB"], removes: [], isConflict: false } },
      localTags: {},
      gitRefs: {},
      gitHeads: {},
    });

    const rootHead = divStore.getOpHeads()[0]!;

    const op1 = divStore.writeOperation({
      id: "op1".padEnd(64, "0"),
      viewId: v1,
      parents: [rootHead],
      metadata: {
        description: "branch 1",
        hostname: "node1",
        username: "user1",
        isSnapshot: false,
        workspaceName: "default",
        time: { startMillis: 100n, endMillis: 200n, tzOffset: 0 },
        attributes: {},
      },
    });

    const op2 = divStore.writeOperation({
      id: "op2".padEnd(64, "0"),
      viewId: v2,
      parents: [rootHead],
      metadata: {
        description: "branch 2",
        hostname: "node2",
        username: "user2",
        isSnapshot: false,
        workspaceName: "default",
        time: { startMillis: 150n, endMillis: 250n, tzOffset: 0 },
        attributes: {},
      },
    });

    // Create divergent heads: op1 and op2
    divStore.updateOpHeads([rootHead], op1);
    divStore.addOpHead(op2);

    expect(divStore.getOpHeads().length).toBe(2);

    const resolution = divStore.resolveOpHeads();
    expect(resolution.wasDivergent).toBe(true);
    expect(resolution.heads).toContain(op1);
    expect(resolution.heads).toContain(op2);

    // After resolution, single head exists
    const currentHeads = divStore.getOpHeads();
    expect(currentHeads.length).toBe(1);
    expect(currentHeads[0]).toBe(resolution.opId);

    // Inspect the reconciled merge operation and merged view
    const reconciledOp = divStore.readOperation(resolution.opId);
    expect(reconciledOp.parents).toContain(op1);
    expect(reconciledOp.parents).toContain(op2);

    const mergedView = divStore.readView(reconciledOp.viewId);
    expect(mergedView.headIds).toContain("commitA");
    expect(mergedView.headIds).toContain("commitB");
    expect(mergedView.localBookmarks["feature1"]).toBeDefined();
    expect(mergedView.localBookmarks["feature2"]).toBeDefined();
  });

  test("Repository methods isJjRepo(), initJj(), and jjOpHeadsStore() bind seamlessly with Git repository", () => {
    const gitDir = join(tmpRoot, "git-jj-repo");
    const repo = Repository.init(gitDir, false);

    expect(repo.isJjRepo()).toBe(false);

    const jjStore = repo.initJj();
    expect(repo.isJjRepo()).toBe(true);
    expect(jjStore.name).toBe("simple_op_heads_store");

    const fetchedStore = repo.jjOpHeadsStore();
    expect(fetchedStore.getOpHeads().length).toBe(1);
  });
});
