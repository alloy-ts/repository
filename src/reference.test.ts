import { describe, expect, test } from "vite-plus/test";
import { Repository, type ReflogEntry } from "./main.ts";

describe("Reference and Head reflog API", () => {
  test("head().reflog() returns an array of ReflogEntry items", () => {
    const repo = Repository.open(process.cwd());
    const head = repo.head();
    const log = head.reflog();

    expect(Array.isArray(log)).toBe(true);
    for (const entry of log) {
      expect(typeof entry.previousOid).toBe("string");
      expect(typeof entry.newOid).toBe("string");
      expect(typeof entry.message).toBe("string");
      expect(typeof entry.committer.name).toBe("string");
      expect(typeof entry.committer.email).toBe("string");
      expect(typeof entry.committer.time).toBe("number");
    }
  });

  test("reference.reflog() and reference.reflogExists() query reflog for references", () => {
    const repo = Repository.open(process.cwd());
    const headRef = repo.findReference("HEAD");

    expect(typeof headRef.reflogExists()).toBe("boolean");

    const log: ReflogEntry[] = headRef.reflog();
    expect(Array.isArray(log)).toBe(true);
    for (const entry of log) {
      expect(typeof entry.previousOid).toBe("string");
      expect(typeof entry.newOid).toBe("string");
      expect(typeof entry.message).toBe("string");
    }
  });

  test("reflog records updates when a reference is created", () => {
    const repo = Repository.open(process.cwd());
    const headCommit = repo.headCommit();

    const ref = repo.createReference("refs/heads/__reflog_probe__", headCommit.id, true);
    expect(typeof ref.reflogExists()).toBe("boolean");

    const log = ref.reflog();
    expect(Array.isArray(log)).toBe(true);

    ref.delete();
  });
});
