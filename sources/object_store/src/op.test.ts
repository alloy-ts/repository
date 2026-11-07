import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { OpStore, Repository } from "../../repository/src/main.ts";

describe("OpStore NAPI bindings & SimpleOpStore parity", () => {
  let tmpRoot: string;
  let opStoreDir: string;
  let opStore: OpStore;

  beforeAll(() => {
    tmpRoot = mkdtempSync(join(tmpdir(), "jj-op-test-"));
    opStoreDir = join(tmpRoot, "op_store");
    opStore = OpStore.init(opStoreDir, "0000000000000000000000000000000000000000");
  });

  afterAll(() => {
    rmSync(tmpRoot, { recursive: true, force: true });
  });

  test("init creates simple_op_store structure and reads root operation and view", () => {
    expect(opStore.name).toBe("simple_op_store");
    const rootOpId = opStore.rootOperationId;
    expect(rootOpId).toBe("0".repeat(64));

    const rootOp = opStore.readOperation(rootOpId);
    expect(rootOp.id).toBe(rootOpId);
    expect(rootOp.viewId).toBe("0".repeat(64));
    expect(rootOp.parents).toEqual([]);

    const rootView = opStore.readView(rootOp.viewId);
    expect(rootView.headIds).toEqual(["0000000000000000000000000000000000000000"]);
    expect(rootView.wcCommitIds["default"]).toBe("0000000000000000000000000000000000000000");
  });

  test("writeView and readView persist and retrieve view definitions", () => {
    const viewId = opStore.writeView({
      id: "",
      headIds: ["head1", "head2"],
      wcCommitIds: { default: "head2" },
      localBookmarks: {
        feature: {
          adds: ["head2"],
          removes: [],
          isConflict: false,
        },
      },
      localTags: {},
      gitRefs: {},
      gitHeads: {},
    });

    expect(typeof viewId).toBe("string");
    expect(viewId.length).toBe(64);

    const fetched = opStore.readView(viewId);
    expect(fetched.id).toBe(viewId);
    expect(fetched.headIds).toEqual(["head1", "head2"]);
    expect(fetched.wcCommitIds["default"]).toBe("head2");
    expect(fetched.localBookmarks["feature"]?.adds).toEqual(["head2"]);
  });

  test("writeOperation and readOperation persist and retrieve operation log entries", () => {
    const rootOpId = opStore.rootOperationId;
    const opId = opStore.writeOperation({
      id: "",
      viewId: "v".repeat(64),
      parents: [rootOpId],
      metadata: {
        description: "transaction: rebase commit",
        hostname: "ci-worker",
        username: "jj-bot",
        isSnapshot: false,
        workspaceName: "default",
        time: {
          startMillis: 2000n,
          endMillis: 2100n,
          tzOffset: 0,
        },
        attributes: {
          engine: "jj",
        },
      },
    });

    expect(typeof opId).toBe("string");
    expect(opId.length).toBe(64);

    const fetched = opStore.readOperation(opId);
    expect(fetched.id).toBe(opId);
    expect(fetched.viewId).toBe("v".repeat(64));
    expect(fetched.parents).toEqual([rootOpId]);
    expect(fetched.metadata.description).toBe("transaction: rebase commit");
    expect(fetched.metadata.attributes["engine"]).toBe("jj");
  });

  test("resolveOperationIdPrefix resolves exact, unique prefix, or ambiguous prefixes", () => {
    const rootOpId = opStore.rootOperationId;
    const fullRes = opStore.resolveOperationIdPrefix(rootOpId);
    expect(fullRes.found).toBe(true);
    expect(fullRes.id).toBe(rootOpId);

    const prefixRes = opStore.resolveOperationIdPrefix(rootOpId.slice(0, 16));
    expect(prefixRes.found).toBe(true);
    expect(prefixRes.id).toBe(rootOpId);

    const nonExistent = opStore.resolveOperationIdPrefix("ffff");
    expect(nonExistent.found).toBe(false);
    expect(nonExistent.ambiguous).toBe(false);
  });

  test("gc prunes unreachable operations and views", () => {
    const rootOpId = opStore.rootOperationId;
    const pruned = opStore.gc([rootOpId], 0);
    expect(typeof pruned).toBe("number");
  });

  test("Repository.prototype.jjOpStore connects directly to underlying repository op_store", () => {
    const gitDir = join(tmpRoot, "git-jj-op-repo");
    const repo = Repository.init(gitDir, false);
    repo.initJj();

    const store = repo.jjOpStore();
    expect(store.name).toBe("simple_op_store");
    expect(store.rootOperationId).toBe("0".repeat(64));
  });
});
