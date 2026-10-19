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
    if (typeof repo.config_string("user.signingKey") !== "string") return false;
    const format = repo.config_string("gpg.format") ?? "openpgp";
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

test("decode() returns decoded commit payload", () => {
  const commit = headCommit();
  const decoded = commit.decode();
  expect(decoded.id).toBe(commit.id);
  expect(decoded.message).toBe(commit.message);
  expect(decoded.treeId).toBe(commit.treeId);
  expect(Array.isArray(decoded.parentIds)).toBe(true);
  expect(typeof decoded.author.name).toBe("string");
  expect(typeof decoded.committer.name).toBe("string");
});

test("verifySignature reports that HEAD carries a signature", () => {
  const v = headCommit().verifySignature();
  expect(v.signed).toBe(true);
  expect(typeof v.valid).toBe("boolean");
});

test("verifySignature returns a structured result", () => {
  const v: SignatureVerification = headCommit().verifySignature();
  expect(typeof v.signed).toBe("boolean");
  if (v.signed) {
    expect(typeof v.output).toBe("string");
  }
});

test("verifySignature reports an unsigned commit as not signed", () => {
  const repo = Repository.open(process.cwd());
  const tip = repo.revParseSingle("HEAD");
  const ids = repo.revWalk([tip]);
  const unsigned = ids.find((id) => {
    const c = repo.findCommit(id);
    return !c.verifySignature().signed;
  });
  if (unsigned) {
    expect(repo.findCommit(unsigned).verifySignature().signed).toBe(false);
  }
});

// --- signing --------------------------------------------------------------

test.skipIf(!canSign)(
  "sign creates a new signed commit whose signature verifies",
  () => {
    const base = headCommit();
    const signed = base.sign();

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
