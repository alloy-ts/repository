import assert from "node:assert";
import {
  identityToRaw,
  parseIdentity,
  parseSignature,
  signatureToIdentity,
  signatureToRaw,
} from "../src/main.ts";

// IdentityRef::from_bytes(b" Taylor Example < taylor@example.com >").unwrap().trim()
const actor = parseIdentity(" Taylor Example < taylor@example.com >");
assert.ok(actor);
assert.equal(actor.name, "Taylor Example");
assert.equal(actor.email, "taylor@example.com");

// SignatureRef::from_bytes(b"Taylor Example <taylor@example.com> 1711398853 +0800").unwrap().trim()
const signature = parseSignature("Taylor Example <taylor@example.com> 1711398853 +0800");
assert.ok(signature);
assert.equal(signature.name, "Taylor Example");
assert.equal(signature.email, "taylor@example.com");

// signature.actor() == actor
assert.deepEqual(signatureToIdentity(signature), actor);

// signature.time().unwrap() -> { seconds: 1711398853, offset: 8 * 60 * 60 }
assert.equal(signature.time, 1_711_398_853);
assert.equal(signature.timezoneOffset, 8 * 60 * 60);

// owned.write_to(&mut out) -> b"Taylor Example <taylor@example.com> 1711398853 +0800"
assert.equal(signatureToRaw(signature), "Taylor Example <taylor@example.com> 1711398853 +0800");

// Bonus: serialize the bare identity (a signature without the timestamp).
assert.equal(identityToRaw(actor), "Taylor Example <taylor@example.com>");

console.log("actor:      ", JSON.stringify(actor));
console.log("signature:  ", JSON.stringify(signature));
console.log("actor():    ", JSON.stringify(signatureToIdentity(signature)));
console.log("write_to:   ", signatureToRaw(signature));
console.log("identity:   ", identityToRaw(actor));
console.log("\nAll gix-actor identity example assertions passed.");
