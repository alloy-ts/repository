import { describe, expect, test } from "vite-plus/test";
import {
  Repository,
  ReplaceNode,
  ReplaceManifest,
  ReplaceOptions,
} from "./main.ts";
import * as fs from "node:fs";
import * as path from "node:path";
import * as os from "node:os";

describe("Repository.replace / ReplaceRefs", () => {
  test("replaces blob object", () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "gix-replace-blob-"));
    try {
      const repo = Repository.init(tmpDir, false);
      const originalBlobId = repo.writeObject("blob", Buffer.from("Original Blob Content"));
      const newBlobId = repo.writeObject("blob", Buffer.from("Replaced Blob Content"));

      const node = new ReplaceNode(newBlobId, "Replacing blob content", "Blob Author", "author@example.com");
      const manifest = new ReplaceManifest(originalBlobId, node);
      const opts = new ReplaceOptions(false);

      const res = repo.replace(manifest, opts);
      expect(res.oid).toBe(newBlobId);

      const refPath = path.join(repo.gitDir(), "refs", "replace", originalBlobId);
      expect(fs.existsSync(refPath)).toBe(true);
      expect(fs.readFileSync(refPath, "utf-8")).toContain(newBlobId);
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  test("replaces tree object", () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "gix-replace-tree-"));
    try {
      const repo = Repository.init(tmpDir, false);
      const blob1 = repo.writeObject("blob", Buffer.from("f1"));
      const blob2 = repo.writeObject("blob", Buffer.from("f2"));

      const tree1 = repo.writeTree([{ path: "f1.txt", id: blob1, mode: "100644" }]);
      const tree2 = repo.writeTree([{ path: "f2.txt", id: blob2, mode: "100644" }]);

      const node = new ReplaceNode(tree2, "Replacing tree object", "Tree Author", "author@example.com");
      const manifest = new ReplaceManifest(tree1, node);
      const opts = new ReplaceOptions(false);

      const res = repo.replace(manifest, opts);
      expect(res.oid).toBe(tree2);

      const refPath = path.join(repo.gitDir(), "refs", "replace", tree1);
      expect(fs.existsSync(refPath)).toBe(true);
      expect(fs.readFileSync(refPath, "utf-8")).toContain(tree2);
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  test("replaces commit object", () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "gix-replace-commit-"));
    try {
      const repo = Repository.init(tmpDir, false);
      const emptyTree = repo.emptyTree();

      const commit1 = repo.newCommit("Initial commit", emptyTree.id, []);
      const commit2 = repo.newCommit("Replacement commit", emptyTree.id, []);

      const node = new ReplaceNode(commit2, "Replacing commit", "Commit Author", "committer@example.com");
      const manifest = new ReplaceManifest(commit1, node);
      const opts = new ReplaceOptions(false);

      const res = repo.replace(manifest, opts);
      expect(res.oid).toBe(commit2);

      const refPath = path.join(repo.gitDir(), "refs", "replace", commit1);
      expect(fs.existsSync(refPath)).toBe(true);
      expect(fs.readFileSync(refPath, "utf-8")).toContain(commit2);
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  test("replaces tag object", () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "gix-replace-tag-"));
    try {
      const repo = Repository.init(tmpDir, false);
      const emptyTree = repo.emptyTree();
      const commitId = repo.newCommit("Commit to tag", emptyTree.id, []);

      const tag1 = repo.writeTag({ name: "v1.0", target: commitId, targetKind: "commit", message: "Tag v1.0" });
      const tag2 = repo.writeTag({ name: "v1.0-replaced", target: commitId, targetKind: "commit", message: "Tag v1.0 Replaced" });

      const node = new ReplaceNode(tag2, "Replacing annotated tag", "Tagger", "tagger@example.com");
      const manifest = new ReplaceManifest(tag1, node);
      const opts = new ReplaceOptions(false);

      const res = repo.replace(manifest, opts);
      expect(res.oid).toBe(tag2);

      const refPath = path.join(repo.gitDir(), "refs", "replace", tag1);
      expect(fs.existsSync(refPath)).toBe(true);
      expect(fs.readFileSync(refPath, "utf-8")).toContain(tag2);
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  test("replaces entry object using commit metadata", () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "gix-replace-entry-"));
    try {
      const repo = Repository.init(tmpDir, false);
      const file1 = path.join(tmpDir, "app.config.json");
      fs.writeFileSync(file1, JSON.stringify({ version: "1.0.0" }));

      const sig = { name: "Alice", email: "alice@example.com", time: 1700000000, timezoneOffset: 0 };
      const commit1 = repo.commitAs(sig, sig, "HEAD", "Initial commit with app.config.json", repo.emptyTree().id, []);

      const tree = repo.findTree(repo.findCommit(commit1).treeId);
      const entry = tree.entryByPath("app.config.json");

      if (entry) {
        const meta = entry.commitMetadata();
        if (meta) {
          expect(meta.authorName).toBe("Alice");
          expect(meta.authorEmail).toBe("alice@example.com");
        }
      }

      const newBlobId = repo.writeObject("blob", Buffer.from(JSON.stringify({ version: "1.0.1" })));
      const initialEntryOid = entry ? entry.id : "1111111111111111111111111111111111111111";

      const node = new ReplaceNode(newBlobId, "Bump config version", "Alice", "alice@example.com");
      const manifest = new ReplaceManifest(initialEntryOid, node, "app.config.json");
      const opts = new ReplaceOptions(false);

      const res = repo.replace(manifest, opts);
      expect(res.oid).toBe(newBlobId);

      const refPath = path.join(repo.gitDir(), "refs", "replace", initialEntryOid);
      expect(fs.existsSync(refPath)).toBe(true);
      expect(fs.readFileSync(refPath, "utf-8")).toContain(newBlobId);
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  test("replaces the same object OID multiple times into a single replace manifest", () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "gix-replace-history-"));
    try {
      const repo = Repository.init(tmpDir, false);
      const initOid = repo.writeObject("blob", Buffer.from("v0 content"));

      const v1Oid = repo.writeObject("blob", Buffer.from("v1 content"));
      const node1 = new ReplaceNode(v1Oid, "Revision 1", "Author 1", "a1@example.com");
      const manifest = new ReplaceManifest(initOid, node1);
      const opts = new ReplaceOptions(false);

      repo.replace(manifest, opts);

      const v2Oid = repo.writeObject("blob", Buffer.from("v2 content"));
      const node2 = new ReplaceNode(v2Oid, "Revision 2", "Author 2", "a2@example.com", v1Oid);
      manifest.addRevision(node2);

      repo.replace(manifest, opts);

      expect(manifest.history.length).toBe(2);
      expect(manifest.headOid).toBe(v2Oid);

      const canonicalRefPath = path.join(repo.gitDir(), "refs", "replace", initOid);
      expect(fs.existsSync(canonicalRefPath)).toBe(true);
      expect(fs.readFileSync(canonicalRefPath, "utf-8")).toContain(v2Oid);

      const v1RefPath = path.join(repo.gitDir(), "refs", "replace", v1Oid);
      const v2RefPath = path.join(repo.gitDir(), "refs", "replace", v2Oid);
      expect(fs.existsSync(v1RefPath)).toBe(true);
      expect(fs.existsSync(v2RefPath)).toBe(true);
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });
});
