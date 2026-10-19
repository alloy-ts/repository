import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { Repository, type ReflogEntry } from "./main.ts";

describe("Reference reflog", () => {
  let base: string;
  let repo: Repository;

  beforeAll(() => {
    base = mkdtempSync(join(tmpdir(), "reflog-test-"));
    repo = Repository.init(base, false);
  });

  afterAll(() => {
    rmSync(base, { recursive: true, force: true });
  });

  test("returns reflog entries when a reference is created/updated", () => {
    const blobId = repo.writeBlob(Buffer.from("reflog file content\n"));
    const commitId = repo.commit("refs/heads/main", "initial commit for reflog test", blobId, []);

    const mainRef = repo.findReference("refs/heads/main");
    const logs: ReflogEntry[] = mainRef.reflog();

    expect(Array.isArray(logs)).toBe(true);
    expect(logs.length).toBeGreaterThan(0);

    const latest = logs[logs.length - 1];
    expect(latest?.newOid).toBe(commitId);
    expect(typeof latest?.message).toBe("string");
    expect(typeof latest?.committer.name).toBe("string");
  });

  test("returns an array for HEAD reflog", () => {
    const headRef = repo.findReference("HEAD");
    const logs = headRef.reflog();
    expect(Array.isArray(logs)).toBe(true);
  });
});
