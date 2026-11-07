import { describe, expect, test } from "vite-plus/test";
import { Cluster, Process, SCHED_NONE, SCHED_RR, Worker } from "../main.ts";

describe("Process constructor extensions", () => {
  test("creates process from string executable", async () => {
    const proc = new Process("echo");
    expect(proc.program).toBe("echo");
    const res = await proc.run();
    expect(res.success).toBe(true);
  });

  test("creates process from file:// URL string", async () => {
    const proc = new Process("file:///bin/echo");
    expect(proc.program).toBe("/bin/echo");
  });

  test("creates process from URL-like or Task-like object", async () => {
    const procFromHref = new Process({ href: "file:///usr/bin/env" });
    expect(procFromHref.program).toBe("/usr/bin/env");

    const procFromPathname = new Process({ pathname: "/bin/sh", args: ["-c", "exit 0"] });
    expect(procFromPathname.program).toBe("/bin/sh");
    expect(procFromPathname.args).toEqual(["-c", "exit 0"]);
  });
});

describe("Cluster scheduling and settings", () => {
  test("exposes scheduling constants", () => {
    expect(SCHED_NONE).toBe(1);
    expect(SCHED_RR).toBe(2);
  });

  test("cluster primary detection and settings", () => {
    const cluster = new Cluster();
    expect(Cluster.isPrimary()).toBe(true);
    expect(Cluster.isWorker()).toBe(false);
    expect(Cluster.isMaster()).toBe(true);
    expect(Cluster.isTask()).toBe(false);

    expect(cluster.isPrimary).toBe(true);
    expect(cluster.isWorker).toBe(false);

    expect(cluster.schedulingPolicy).toBe(SCHED_RR);

    Cluster.setupPrimary({
      exec: "task.js",
      args: ["--mode", "worker"],
      silent: true,
    });

    const settings = Cluster.settings();
    expect(settings.exec).toBe("task.js");
    expect(settings.args).toEqual(["--mode", "worker"]);
    expect(settings.silent).toBe(true);
  });

  test("Worker instances and methods", () => {
    const worker = new Worker(42, 12345);
    expect(worker.id).toBe(42);
    expect(worker.process.pid).toBe(12345);
    expect(worker.isConnected()).toBe(true);
    expect(worker.isDead()).toBe(false);
    expect(worker.exitedAfterDisconnect).toBeFalsy();

    let received: unknown = null;
    worker.on("message", (msg) => {
      received = msg;
    });
    expect(worker.send("hello")).toBe(true);
    expect(received).toBe("hello");

    let disconnected = false;
    worker.on("disconnect", () => {
      disconnected = true;
    });
    worker.disconnect();
    expect(disconnected).toBe(true);
    expect(worker.isConnected()).toBe(false);

    worker.emit("exit", { code: 0 });
    expect(worker.isDead()).toBe(true);
    expect(worker.exitedAfterDisconnect).toBe(true);
  });

  test("Cluster fork and registry tracking", () => {
    const cluster = new Cluster();
    let forkNotified = false;
    Cluster.on("fork", (w: unknown) => {
      forkNotified = true;
      expect((w as Worker).id).toBeDefined();
    });

    const worker = Cluster.fork({ CLUSTER_WORKER_TEST: "true" });
    expect(forkNotified).toBe(true);
    expect(typeof worker.id).toBe("number");
    expect(worker.isConnected()).toBe(true);

    const workers = cluster.workers as Record<string, Worker>;
    expect(workers[String(worker.id)]).toBeDefined();

    worker.emit("exit", { code: 0 });
    const updated = cluster.workers as Record<string, Worker>;
    expect(updated[String(worker.id)]).toBeUndefined();
  });

  test("Cluster disconnect all workers", () => {
    const w1 = Cluster.fork();
    const w2 = Cluster.fork();

    expect(w1.isConnected()).toBe(true);
    expect(w2.isConnected()).toBe(true);

    let callbackCalled = false;
    Cluster.disconnect(() => {
      callbackCalled = true;
    });

    expect(callbackCalled).toBe(true);
    expect(w1.isConnected()).toBe(false);
    expect(w2.isConnected()).toBe(false);
  });
});
