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
 * Whether the repository is configured to sign commits. Signing needs
 * `user.signingKey` (and a matching `gpg.format` / installed signer program).
 */
function signingConfigured(): boolean {
  try {
    return (
      typeof Repository.open(process.cwd()).config_string("user.signingKey") ===
      "string"
    );
  } catch {
    return false;
  }
}

const canSign = signingConfigured();

// --- verifying signatures -------------------------------------------------

test("verifySignature reports an unsigned commit as not signed", () => {
  const v = headCommit().verifySignature();
  expect(v.signed).toBe(false);
  expect(v.status).toBeNull();
  expect(v.signer).toBeNull();
  expect(v.key).toBeNull();
  expect(v.fingerprint).toBeNull();
  expect(v.valid).toBeNull();
});

test("verifySignature returns a structured result for HEAD", () => {
  const v: SignatureVerification = headCommit().verifySignature();
  expect(v).toMatchObject({ signed: false });
});

// --- signing --------------------------------------------------------------

// Signing requires a configured signer (`gpg.format` / `user.signingKey` and an
// installed gpg/ssh-keygen). Skipped in environments without one so the suite
// stays green; run locally with signing configured to exercise the full path.
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

test("sign surfaces a clear error when no signer is configured", () => {
  if (canSign) {
    // Configured: signing should succeed (covered by the test above).
    expect(() => headCommit().sign()).not.toThrow();
  } else {
    expect(() => headCommit().sign()).toThrow();
  }
});
