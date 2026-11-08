import { describe, expect, test } from "vite-plus/test";
import { cluster } from "../main.ts";

describe("Cluster", () => {
  test("exposes isPrimary, isMaster, isWorker, and istask", () => {
    expect(cluster.isPrimary).toBe(true);
    expect(cluster.isMaster).toBe(true);
    expect(cluster.isWorker).toBe(false);
    expect(cluster.istask).toBe(false);
  });

  test("exposes scheduling policies", () => {
    expect(cluster.schedRr).toBe(2);
    expect(cluster.schedNone).toBe(1);
    expect([1, 2]).toContain(cluster.schedulingPolicy);
  });

  test("setupPrimary updates settings and emits setup event", () => {
    let setupFired = false;
    cluster.on("setup", () => {
      setupFired = true;
    });

    cluster.setupPrimary({
      exec: "test_worker.js",
      args: ["--port", "8000"],
      silent: true,
    });

    const settings = cluster.settings;
    expect(settings.exec).toBe("test_worker.js");
    expect(settings.args).toEqual(["--port", "8000"]);
    expect(settings.silent).toBe(true);
    expect(setupFired).toBe(true);
  });

  test("forks a worker and exposes task properties", () => {
    const task = cluster.fork();
    expect(typeof task.id).toBe("number");
    expect(task.id).toBeGreaterThan(0);
    expect(task.isDead()).toBe(false);
    expect(task.isConnected()).toBe(true);
    expect(task.exitedAfterDisconnect).toBe(false);

    const tasks = cluster.tasks as Record<string, { id: number }>;
    expect(typeof tasks).toBe("object");
    expect(tasks[String(task.id)]).toBeDefined();
    expect(tasks[String(task.id)]?.id).toBe(task.id);

    // Terminate the worker
    expect(task.kill("SIGTERM")).toBe(true);
  });

  test("disconnects tasks gracefully", () => {
    let callbackCalled = false;
    cluster.disconnect(() => {
      callbackCalled = true;
    });
    expect(typeof cluster.disconnect).toBe("function");
    expect(callbackCalled).toBe(false);
  });
});
