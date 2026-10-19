import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { expect, test } from "vite-plus/test";
import { Repository, type Commit, type SignatureVerification } from "./main.ts";

/** The commit `HEAD` points to in the current working-tree repository. */
function headCommit(): Commit {
  return Repository.open(process.cwd()).headCommit();
}

/**
 * Helper to create a temporary repository with a single unsigned commit.
 */
function createRepoWithUnsignedCommit(): { repo: Repository; commit: Commit; cleanup: () => void } {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "repo-test-commit-"));
  const repo = Repository.init(tmpDir, false);
  const currentHeadTree = headCommit().treeId;
  const commitId = repo.commit("refs/heads/main", "initial commit", currentHeadTree, []);
  const commit = repo.findCommit(commitId);
  const cleanup = () => {
    try {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    } catch {}
  };
  return { repo, commit, cleanup };
}

/**
 * Whether the repository is configured to sign commits. Signing needs
 * `user.signingKey` (and a matching `gpg.format` / installed signer program).
 */
function signingConfigured(): boolean {
  try {
    return typeof Repository.open(process.cwd()).configString("user.signingKey") === "string";
  } catch {
    return false;
  }
}

const canSign = signingConfigured();

// --- verifying signatures -------------------------------------------------

test("verifySignature reports an unsigned commit as not signed", () => {
  const { commit, cleanup } = createRepoWithUnsignedCommit();
  try {
    const v = commit.verifySignature();
    expect(v.signed).toBe(false);
    expect(v.status).toBeUndefined();
    expect(v.signer).toBeUndefined();
    expect(v.key).toBeUndefined();
    expect(v.fingerprint).toBeUndefined();
    expect(v.valid).toBeUndefined();
  } finally {
    cleanup();
  }
});

test("verifySignature returns a structured result for HEAD", () => {
  const v: SignatureVerification = headCommit().verifySignature();
  expect(typeof v.signed).toBe("boolean");
  if (v.signed) {
    expect(typeof v.format).toBe("string");
    expect(typeof v.valid).toBe("boolean");
  } else {
    expect(v.status).toBeUndefined();
  }
});

// --- signing --------------------------------------------------------------

// Signing requires a configured signer (`gpg.format` / `user.signingKey` and an
// installed gpg/ssh-keygen). Skipped in environments without one so the suite
// stays green; run locally with signing configured to exercise the full path.
test.skipIf(!canSign)("sign creates a new signed commit whose signature verifies", () => {
  const base = headCommit();
  const signed = base.sign();

  // A signature changes the object id, so the signed commit is distinct.
  expect(signed.id).not.toBe(base.id);

  const v = signed.verifySignature();
  expect(v.signed).toBe(true);
  expect(v.valid).toBe(true);
  expect(v.signer).not.toBeNull();
  expect(typeof v.status).toBe("string");
});

test("sign surfaces a clear error when no signer is configured", () => {
  if (canSign) {
    // Configured: signing should succeed (covered by the test above).
    expect(() => headCommit().sign()).not.toThrow();
  } else {
    expect(() => headCommit().sign()).toThrow();
  }
});
