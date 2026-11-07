import { describe, expect, test } from "vite-plus/test";
import { Cluster, createCluster } from "../main.ts";

describe("Cluster (src/task/cluster.rs)", () => {
  test("Cluster primary properties and defaults", () => {
    const cluster = createCluster();
    expect(cluster.isPrimary).toBe(true);
    expect(cluster.isMaster).toBe(true);
    expect(cluster.isWorker).toBe(false);
    expect(cluster.isTask).toBe(false);
    expect(cluster.schedulingPolicy).toBe(cluster.schedRr);
  });

  test("Cluster fork creates task with process handle", () => {
    const cluster = new Cluster();
    const task = cluster.fork();
    expect(task.id).toBeGreaterThan(0);
    expect(task.isConnected()).toBe(true);
    expect(task.isDead()).toBe(false);
    expect(task.exitedAfterDisconnect).toBe(false);

    expect(task.process).toBeDefined();
    expect(task.process?.pid).toBeGreaterThan(0);

    const sent = task.send({ hello: "worker" });
    expect(sent).toBe(true);

    task.disconnect();
    expect(task.exitedAfterDisconnect).toBe(true);
    expect(task.isConnected()).toBe(false);

    task.kill();
    expect(task.isDead()).toBe(true);
  });

  test("Cluster setupPrimary updates settings", () => {
    const cluster = new Cluster();
    cluster.setupPrimary({
      exec: "worker.js",
      args: ["--arg1"],
      silent: true,
    });
    cluster.disconnect();
  });
});
