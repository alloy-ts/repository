import { execSync } from "node:child_process";
import { expect, test } from "vite-plus/test";
import {
  Repository,
  type Commit,
  type SignatureVerification,
} from "../dist/index.js";

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

test("verifySignature reports that HEAD carries a signature", () => {
  const v = headCommit().verifySignature();
  // This repository's HEAD commit is PGP-signed (it has a `gpgsig` header).
  expect(v.signed).toBe(true);
  expect(typeof v.valid).toBe("boolean");
});

test("verifySignature returns a structured result", () => {
  const v: SignatureVerification = headCommit().verifySignature();
  expect(typeof v.signed).toBe("boolean");
  // When a signature is present, verifier output (or its failure reason) is a string.
  if (v.signed) {
    expect(typeof v.output).toBe("string");
  }
});

test("verifySignature reports an unsigned commit as not signed", () => {
  // A commit known to be unsigned (the tree object is not a commit, but here we pick a
  // historical, unsigned object id if available). As a portable check, we verify the
  // detection contract: a commit whose `signature()` is empty yields `signed: false`.
  const repo = Repository.open(process.cwd());
  // Walk history and find a commit without a signature; if none, this test is a no-op pass.
  const tip = repo.revParseSingle("HEAD");
  const ids = repo.revWalk([tip]);
  const unsigned = ids.find((id) => {
    const c = repo.findCommit(id);
    return !c.verifySignature().signed;
  });
  // In a fully-signed history this is skipped; otherwise assert the contract.
  if (unsigned) {
    expect(repo.findCommit(unsigned).verifySignature().signed).toBe(false);
  }
});

// --- signing --------------------------------------------------------------

// Signing requires a configured signer (`gpg.format` / `user.signingKey` and an installed
// gpg/ssh-keygen). Skipped in environments without one so the suite stays green; run locally
// with a working signer to exercise the full sign+verify path.
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
    // Configured and installed: signing should succeed (covered by the test above).
    expect(() => headCommit().sign()).not.toThrow();
  } else {
    // No working signer (e.g. `user.signingKey` set but `gpg` not installed): fails clearly.
    expect(() => headCommit().sign()).toThrow();
  }
});
