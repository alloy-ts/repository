import { execSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vite-plus/test";
import { namespaceExpand, Repository } from "./main.ts";

let fixture: string;
let repo: Repository;

function git(args: string) {
  execSync(`git ${args}`, { cwd: fixture, stdio: "ignore" });
}

beforeAll(() => {
  fixture = mkdtempSync(join(tmpdir(), "repo-namespace-"));
  git("init --initial-branch=main");
  git("config user.email test@example.com");
  git("config user.name Test");
  git("config commit.gpgsign false");
  git("commit --allow-empty -m seed");
  repo = Repository.open(fixture);
});

afterAll(() => {
  rmSync(fixture, { recursive: true, force: true });
});

describe("Reference namespaces (src/namespace.rs)", () => {
  test("namespaceExpand() canonicalizes namespace names", () => {
    expect(namespaceExpand("foo")).toBe("refs/namespaces/foo/");
    expect(namespaceExpand("foo/bar")).toBe("refs/namespaces/foo/refs/namespaces/bar/");
    expect(() => namespaceExpand("refs/namespaces/x/")).toThrow();
  });

  test("namespace()/setNamespace()/clearNamespace() round-trip in canonical form", () => {
    repo.clearNamespace();
    expect(repo.namespace()).toBe(null);

    expect(repo.setNamespace("ns1")).toBe(null);
    expect(repo.namespace()).toBe("refs/namespaces/ns1/");

    // Nested names expand per component, like git.
    repo.setNamespace("a/b");
    expect(repo.namespace()).toBe("refs/namespaces/a/refs/namespaces/b/");

    // Invalid namespaces throw and leave the previous value untouched.
    expect(() => repo.setNamespace("refs/namespaces/x/")).toThrow();
    expect(repo.namespace()).toBe("refs/namespaces/a/refs/namespaces/b/");

    // clearNamespace() returns the previously set (canonical) value.
    expect(repo.clearNamespace()).toBe("refs/namespaces/a/refs/namespaces/b/");
    expect(repo.namespace()).toBe(null);
  });

  test("nameWithoutNamespace() strips canonical namespace prefixes", () => {
    const head = repo.headId();

    repo.setNamespace("ns1");
    try {
      // With a namespace set, refs are physically written under
      // refs/namespaces/<ns>/ but presented as if the namespace were the root.
      const created = repo.createReference("refs/heads/__probe_ns__", head, true);
      expect(created.name).toBe("refs/heads/__probe_ns__");
      expect(repo.findReference("refs/heads/__probe_ns__").name).toBe("refs/heads/__probe_ns__");
      // The presented name is outside the namespace, so stripping finds nothing.
      expect(created.nameWithoutNamespace("ns1")).toBe(null);
    } finally {
      repo.clearNamespace();
    }

    // Outside the namespace the physical, namespaced path is visible.
    const physical = repo.tryFindReference("refs/namespaces/ns1/refs/heads/__probe_ns__");
    expect(physical?.name).toBe("refs/namespaces/ns1/refs/heads/__probe_ns__");
    // The short namespace name is expanded to refs/namespaces/ns1/ and stripped.
    expect(physical!.nameWithoutNamespace("ns1")).toBe("refs/heads/__probe_ns__");
    // A different namespace does not match -> null.
    expect(physical!.nameWithoutNamespace("other")).toBe(null);
    physical!.delete();
  });
});
