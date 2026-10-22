import { describe, expect, test } from "vite-plus/test";
import type { RefInfo, Replace, ReplaceManifest } from "./main.ts";
import { Repository } from "./main.ts";

describe("Replace / ReplaceManifest (src/replace.rs)", () => {
  test("Replace structure handles fields", () => {
    const ref: RefInfo = {
      name: "refs/replace/1111111111111111111111111111111111111111",
      target: "2222222222222222222222222222222222222222",
    };

    const node: Replace = {
      this: ref,
      prev: undefined,
      next: undefined,
      head: ref,
      authorName: "Alice",
      authorEmail: "alice@example.com",
      updatedAt: "2026-10-20T12:00:00Z",
    };

    expect(node.this.name).toBe("refs/replace/1111111111111111111111111111111111111111");
    expect(node.authorName).toBe("Alice");
    expect(node.authorEmail).toBe("alice@example.com");
    expect(node.updatedAt).toBe("2026-10-20T12:00:00Z");
  });

  test("ReplaceManifest structure holds revisions array", () => {
    const ref: RefInfo = {
      name: "refs/replace/1111111111111111111111111111111111111111",
      target: "2222222222222222222222222222222222222222",
    };

    const node: Replace = {
      this: ref,
      prev: undefined,
      next: undefined,
      head: ref,
      authorName: "Bob",
      authorEmail: "bob@example.com",
      updatedAt: "2026-10-20T12:00:00Z",
    };

    const manifest: ReplaceManifest = {
      id: "1111111111111111111111111111111111111111",
      type: "blob",
      revisions: [node],
      head: ref,
      path: undefined,
    };

    expect(manifest.id).toBe("1111111111111111111111111111111111111111");
    expect(manifest.type).toBe("blob");
    expect(manifest.revisions.length).toBe(1);
    expect(manifest.revisions[0].authorName).toBe("Bob");
  });

  test("getReplaceNode returns null when replace ref does not exist", () => {
    const repo = Repository.init("/tmp/replace-test-repo-" + Date.now());
    const node = repo.getReplaceNode("1111111111111111111111111111111111111111");
    expect(node).toBeNull();
  });

  test("getReplaceManifest returns null when replace ref does not exist", () => {
    const repo = Repository.init("/tmp/replace-manifest-test-repo-" + Date.now());
    const manifest = repo.getReplaceManifest("1111111111111111111111111111111111111111");
    expect(manifest).toBeNull();
  });
});
