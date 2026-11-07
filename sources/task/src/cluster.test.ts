import { describe, expect, test } from "vite-plus/test";
import { Cluster } from "../../repository/src/main.ts";

const command = (source: string) => ({
  program: process.execPath,
  args: ["-e", source],
  timeoutMs: 5000,
});

describe("Cluster", () => {
  test("returns stable IDs and passes each ID to its actual child", async () => {
    const cluster = new Cluster();
    const first = cluster.fork(command("process.stdout.write(process.env.ALLOY_CLUSTER_TASK_ID)"));
    const second = cluster.fork(command("process.stdout.write(process.env.ALLOY_CLUSTER_TASK_ID)"));
    try {
      expect(first.id).toBe(1);
      expect(second.id).toBe(2);
      expect(cluster.taskIds).toEqual([1, 2]);
      const results = await Promise.all([first.run(), second.run()]);
      expect(results.map((result) => result.output!.stdout.toString())).toEqual(["1", "2"]);
      expect(results.every((result) => result.success)).toBe(true);
      const snapshot = cluster.taskIds;
      snapshot.pop();
      expect(cluster.taskIds).toEqual([1, 2]);
    } finally {
      cluster.remove(first.id!);
      cluster.remove(second.id!);
    }
  });

  test("cancels only the selected child and removes registrations", async () => {
    const cluster = new Cluster();
    const first = cluster.fork(command("setInterval(() => {}, 1000)"));
    const second = cluster.fork(command("process.stdout.write('unaffected')"));
    const completions = [first.run(), second.run()];
    try {
      expect(cluster.cancel(first.id!)).toBe(true);
      expect(cluster.cancel(999)).toBe(false);
      const [cancelled, completed] = await Promise.all(completions);
      expect(cancelled.failure!.kind).toBe("cancelled");
      expect(completed.success).toBe(true);
      expect(completed.output!.stdout.toString()).toBe("unaffected");
      expect(cluster.remove(first.id!)).toBe(true);
      expect(cluster.remove(first.id!)).toBe(false);
      expect(cluster.taskIds).toEqual([second.id]);
    } finally {
      cluster.cancelAll();
      await Promise.allSettled(completions);
      cluster.remove(first.id!);
      cluster.remove(second.id!);
    }
  });

  test("removal cancels running work and does not reuse its ID", async () => {
    const cluster = new Cluster();
    const first = cluster.fork(command("setInterval(() => {}, 1000)"));
    const completion = first.run();
    expect(cluster.remove(first.id!)).toBe(true);
    expect((await completion).failure!.kind).toBe("cancelled");
    const second = cluster.fork(command("process.stdout.write('next')"));
    try {
      expect(second.id).toBeGreaterThan(first.id!);
      expect((await second.run()).output!.stdout.toString()).toBe("next");
    } finally {
      cluster.remove(second.id!);
    }
  });
});

test("dropping a collected Cluster cancels an already-started child", async () => {
  const { mkdtemp, rm } = await import("node:fs/promises");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const { Process } = await import("../../repository/src/main.ts");
  const directory = await mkdtemp(join(tmpdir(), "alloy-cluster-drop-"));
  const marker = join(directory, "started");
  const moduleUrl = new URL("../../../dist/index.js", import.meta.url).href;
  const source = `
    (async () => {
      const { Cluster } = await import(${JSON.stringify(moduleUrl)});
      const { access } = await import('node:fs/promises');
      async function launch() {
        const cluster = new Cluster();
        const child = cluster.fork({
          program: process.execPath,
          args: ['-e', "require('node:fs').writeFileSync(process.argv[1], 'ready'); setInterval(() => {}, 1000)", ${JSON.stringify(marker)}],
          timeoutMs: 5000,
        });
        const deadline = Date.now() + 3000;
        while (true) {
          try { await access(${JSON.stringify(marker)}); break; } catch {}
          if (Date.now() >= deadline) throw new Error('child did not start');
          await new Promise(resolve => setTimeout(resolve, 10));
        }
        // Keep the cluster reachable until the child has actually started.
        if (!cluster.taskIds.includes(child.id)) throw new Error('missing child');
        return child;
      }
      const child = await launch();
      const timer = setInterval(() => global.gc(), 10);
      try {
        const result = await child.run();
        if (result.failure?.kind !== 'cancelled') throw new Error(JSON.stringify(result));
        process.stdout.write('cancelled');
      } finally { clearInterval(timer); child.cancel(); }
    })().catch(error => { console.error(error); process.exitCode = 1; });
  `;
  const parent = new Process({
    program: process.execPath,
    args: ["--expose-gc", "-e", source],
    timeoutMs: 10000,
  });
  try {
    const result = await parent.run();
    expect(result.success).toBe(true);
    expect(result.output!.success).toBe(true);
    expect(result.output!.stdout.toString()).toBe("cancelled");
  } finally {
    parent.cancel();
    await rm(directory, { recursive: true, force: true });
  }
}, 15000);
