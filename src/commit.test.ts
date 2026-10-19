import { execSync } from "node:child_process";
import { expect, test } from "vite-plus/test";
import {
  Repository,
  type Commit,
  type SignatureVerification,
} from "./main.ts";

/** The commit `HEAD` points to in the current working-tree repository. */
function headCommit(): Commit {
  return Repository.open(process.cwd()).headCommit();
}

/**
 * Whether signing can actually run here: it needs both a configured `user.signingKey`
 * *and* an installed signer program (`gpg` / `gpgsm` / `ssh-keygen`) for the configured
 * `gpg.format` (default `openpgp`).
 */
function signerAvailable(): boolean {
  try {
    const repo = Repository.open(process.cwd());
    if (typeof repo.configString("user.signingKey") !== "string") return false;
    const format = repo.configString("gpg.format") ?? "openpgp";
    const program =
      format === "ssh" ? "ssh-keygen" : format === "x509" ? "gpgsm" : "gpg";
    execSync(`command -v ${program}`, { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
}

const canSign = signerAvailable();

// --- verifying signatures -------------------------------------------------

test("verifySignature reports that HEAD carries a signature or handles verification", () => {
  const commit = headCommit();
  const v = commit.verifySignature();
  expect(typeof v.signed).toBe("boolean");
  if (v.signed) {
    expect(typeof v.valid).toBe("boolean");
    if (v.output !== undefined && v.output !== null) {
      expect(typeof v.output).toBe("string");
    }
  }
});

test("verifySignature returns a structured SignatureVerification object", () => {
  const v: SignatureVerification = headCommit().verifySignature();
  expect(typeof v.signed).toBe("boolean");
  if (v.signed) {
    if (v.format !== undefined) expect(typeof v.format).toBe("string");
    if (v.status !== undefined) expect(typeof v.status).toBe("string");
    if (v.trust !== undefined) expect(typeof v.trust).toBe("string");
    if (v.signer !== undefined) expect(typeof v.signer).toBe("string");
    if (v.key !== undefined) expect(typeof v.key).toBe("string");
    if (v.fingerprint !== undefined) expect(typeof v.fingerprint).toBe("string");
    if (v.valid !== undefined) expect(typeof v.valid).toBe("boolean");
    if (v.output !== undefined) expect(typeof v.output).toBe("string");
  }
});

test("verifySignature reports a newly created unsigned commit as not signed", () => {
  const repo = Repository.open(process.cwd());
  const head = repo.headCommit();
  const newCommitId = repo.commit(
    "refs/heads/__unsigned_probe__",
    "Unsigned commit test message",
    head.treeId,
    [head.id],
  );
  const newCommit = repo.findCommit(newCommitId);
  const v = newCommit.verifySignature();
  expect(v.signed).toBe(false);

  // Cleanup reference
  repo.findReference("refs/heads/__unsigned_probe__").delete();
});

test("commit contains author and committer signatures", () => {
  const commit = headCommit();
  const author = commit.author;
  const committer = commit.committer;

  expect(typeof author.name).toBe("string");
  expect(typeof author.email).toBe("string");
  expect(typeof author.time).toBe("number");
  expect(typeof author.timezoneOffset).toBe("number");

  expect(typeof committer.name).toBe("string");
  expect(typeof committer.email).toBe("string");
  expect(typeof committer.time).toBe("number");
  expect(typeof committer.timezoneOffset).toBe("number");
});

// --- signing --------------------------------------------------------------

test.skipIf(!canSign)(
  "sign creates a new signed commit whose signature verifies",
  () => {
    const base = headCommit();
    const signed = base.sign();

    // A signature changes the object id, so the signed commit is distinct.
    expect(signed.id).not.toBe(base.id);

    const v = signed.verifySignature();
    expect(v.signed).toBe(true);
    expect(v.valid).toBe(true);
    expect(v.signer).not.toBeNull();
    expect(typeof v.status).toBe("string");
  },
);

test("sign surfaces a clear error when no signer is available", () => {
  if (canSign) {
    expect(() => headCommit().sign()).not.toThrow();
  } else {
    expect(() => headCommit().sign()).toThrow();
  }
});
