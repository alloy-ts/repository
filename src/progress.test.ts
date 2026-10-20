import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { MessageLevel, RemoteProgress, Repository, type ProgressEvent } from "./main.ts";

// NOTE: this file exercises the NAPI bindings added for the progress module
// (`RemoteProgress`, `ProgressEvent`, and the `progress` callback on
// `Remote.fetch` / `Repository.clone`). Run `npm run build` first so the
// generated `main.ts` / `index.d.ts` contain `ProgressEvent` and the callback
// overloads.

const base = mkdtempSync(join(tmpdir(), "repo-progress-"));
let source: string;
let cloneDir: string;
let repo: Repository;

function git(dir: string, args: string) {
  execSync(`git ${args}`, { cwd: dir, stdio: "ignore" });
}

// Build a small source repo, clone it (so `repo` has an `origin` pointing at
// `source`), then add a second commit to `source`. Fetching `origin` afterwards
// transfers a pack and therefore drives the progress callback.
beforeAll(async () => {
  source = join(base, "source");
  git(base, `init --quiet ${JSON.stringify(source)}`);
  git(source, `config user.email progress-test@local`);
  git(source, `config user.name progress-test`);
  writeFileSync(join(source, "a.txt"), "a\n");
  git(source, `add a.txt`);
  git(source, `commit -qm init`);

  cloneDir = join(base, "clone");
  repo = await Repository.clone(source, cloneDir, false);

  writeFileSync(join(source, "b.txt"), "b\n");
  git(source, `add b.txt`);
  git(source, `commit -qm second`);
});

afterAll(() => {
  rmSync(base, { recursive: true, force: true });
});

describe("RemoteProgress.fromBytes", () => {
  test("parses an action with percent, step and max", () => {
    const p = RemoteProgress.fromBytes("Receiving objects:  42% (21/50)");
    expect(p).not.toBeNull();
    expect(p!.action).toBe("Receiving objects");
    expect(p!.percent).toBe(42);
    expect(p!.step).toBe(21);
    expect(p!.max).toBe(50);
  });

  test("parses an action with step/max but no percent", () => {
    const p = RemoteProgress.fromBytes("Resolving deltas: 21/50");
    expect(p).not.toBeNull();
    expect(p!.action).toBe("Resolving deltas");
    expect(p!.percent).toBeNull();
    expect(p!.step).toBe(21);
    expect(p!.max).toBe(50);
  });

  test("returns null for a line without progress information", () => {
    // No colon → no action prefix → nothing to parse.
    expect(RemoteProgress.fromBytes("some keep-alive text")).toBeNull();
  });
});

describe("Remote.fetch progress callback", () => {
  test("invokes the callback with well-formed ProgressEvents", async () => {
    const events: ProgressEvent[] = [];
    const outcome = await repo.findRemote("origin").fetch((e: ProgressEvent) => {
      events.push(e);
    });

    // The call completes and returns the documented boolean flags.
    expect(typeof outcome.packReceived).toBe("boolean");
    expect(typeof outcome.negotiated).toBe("boolean");

    // The second commit in `source` makes the fetch transfer progress events.
    expect(events.length).toBeGreaterThan(0);
    for (const e of events) {
      expect(typeof e).toBe("object");
      if (e.name !== null && e.name !== undefined) expect(typeof e.name).toBe("string");
      if (e.step !== null && e.step !== undefined) expect(typeof e.step).toBe("number");
      if (e.max !== null && e.max !== undefined) expect(typeof e.max).toBe("number");
      if (e.percent !== null && e.percent !== undefined) {
        expect(typeof e.percent).toBe("number");
        expect(e.percent).toBeGreaterThanOrEqual(0);
        expect(e.percent).toBeLessThanOrEqual(100);
      }
      if (e.message !== null && e.message !== undefined) expect(typeof e.message).toBe("string");
      if (e.level !== null && e.level !== undefined) {
        expect([MessageLevel.Info, MessageLevel.Success, MessageLevel.Failure]).toContain(e.level);
      }
    }
  });
});

describe("Repository.clone progress callback", () => {
  test("invokes the callback with well-formed ProgressEvents", async () => {
    const events: ProgressEvent[] = [];
    const target = join(base, "cloned");
    const r = await Repository.clone(source, target, false, (e: ProgressEvent) => {
      events.push(e);
    });

    // A Repository is returned...
    expect(r).toBeInstanceOf(Repository);
    expect(Array.isArray(r.remotes())).toBe(true);

    // ...and cloning is chatty enough to emit progress.
    expect(events.length).toBeGreaterThan(0);
    for (const e of events) {
      expect(typeof e).toBe("object");
      if (e.name !== null && e.name !== undefined) expect(typeof e.name).toBe("string");
      if (e.step !== null && e.step !== undefined) expect(typeof e.step).toBe("number");
      if (e.max !== null && e.max !== undefined) expect(typeof e.max).toBe("number");
      if (e.percent !== null && e.percent !== undefined) {
        expect(typeof e.percent).toBe("number");
        expect(e.percent).toBeGreaterThanOrEqual(0);
        expect(e.percent).toBeLessThanOrEqual(100);
      }
      if (e.message !== null && e.message !== undefined) expect(typeof e.message).toBe("string");
      if (e.level !== null && e.level !== undefined) {
        expect([MessageLevel.Info, MessageLevel.Success, MessageLevel.Failure]).toContain(e.level);
      }
    }
  });
});
