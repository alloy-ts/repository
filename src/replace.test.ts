import { describe, expect, test } from "vitest";
import { Repository, replace, tryReplace, getReplaceManifest, getReplaceNode, derefReplace, useReplaceRefs, getReplaceRef } from "./main.js";
import * as fs from "fs";
import * as path from "path";
import * as os from "os";

function createTempRepo(): { repo: Repository; dir: string } {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "repo-replace-test-"));
  const repo = Repository.init(dir);
  return { repo, dir };
}

describe("Git Object Replacement (refs/replace/*)", () => {
  test("replace & tryReplace with replace refs enabled", () => {
    const { repo, dir } = createTempRepo();
    try {
      const oldOid = repo.writeBlob(Buffer.from("initial content"));
      expect(oldOid.length).toBe(40);

      expect(useReplaceRefs(repo)).toBe(true);

      const newOid = replace(repo, oldOid, Buffer.from("replacement v1"), {
        authorName: "Alice",
        authorEmail: "alice@example.com",
      });
      expect(newOid.length).toBe(40);
      expect(newOid).not.toBe(oldOid);

      // Manifest checks
      const manifest = getReplaceManifest(repo, oldOid);
      expect(manifest).not.toBeNull();
      expect(manifest?.id).toBe(oldOid);
      expect(manifest?.head).toBe(newOid);
      expect(manifest?.size).toBe(1);
      expect(manifest?.versions[0].authorName).toBe("Alice");

      // Node lookup
      const node = getReplaceNode(repo, oldOid, newOid);
      expect(node).not.toBeNull();
      expect(node?.id).toBe(newOid);

      // Deref replace
      expect(derefReplace(repo, oldOid)).toBe(newOid);

      // Read blob content should yield replacement
      const blob = repo.findBlob(oldOid);
      expect(blob.content().toString()).toBe("replacement v1");

      // Replace v2 with force
      const newOid2 = tryReplace(repo, oldOid, "replacement v2", { force: true });
      expect(derefReplace(repo, oldOid)).toBe(newOid2);

      const manifest2 = getReplaceManifest(repo, oldOid);
      expect(manifest2?.size).toBe(2);
      expect(manifest2?.head).toBe(newOid2);
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });
});
