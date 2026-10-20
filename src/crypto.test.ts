import { describe, expect, test } from "vite-plus/test";
import { webcrypto } from "node:crypto";
import {
  CryptoKey,
  Signature,
  MultiSignature,
  SignatureBuilder,
  SignatureBytes,
  SignatureScheme,
  keyFormats,
  allKeyFormats,
} from "./main.ts";

const SIG_64 = Buffer.alloc(64, 7);

describe("SignatureBytes", () => {
  test("wraps raw bytes and exposes accessors", () => {
    const sig = new SignatureBytes(Buffer.from([1, 2, 3, 4]));
    expect(sig.bytes).toEqual(Buffer.from([1, 2, 3, 4]));
    expect(sig.asBytes()).toEqual(Buffer.from([1, 2, 3, 4]));
    expect(sig.length).toBe(4);
    expect(sig.isEmpty).toBe(false);
    expect(sig.hex).toBe("01020304");
  });

  test("empty", () => {
    const sig = new SignatureBytes(Buffer.from([]));
    expect(sig.isEmpty).toBe(true);
    expect(sig.length).toBe(0);
    expect(sig.hex).toBe("");
  });
});

describe("SignatureScheme", () => {
  test("from a codec name", () => {
    const s = SignatureScheme.fromCodec("eddsa-msig");
    expect(s.name).toBe("eddsa-msig");
    expect(typeof s.code).toBe("number");
  });

  test("from a numeric codec round-trips with the name", () => {
    const byName = SignatureScheme.fromCodec("eddsa-msig");
    const byCode = SignatureScheme.fromCodec(byName.code);
    expect(byCode.name).toBe("eddsa-msig");
    expect(byCode.code).toBe(byName.code);
  });

  test("rejects an unknown codec", () => {
    expect(() => SignatureScheme.fromCodec("not-a-real-codec")).toThrow();
  });
});

describe("SignatureBuilder / MultiSignature", () => {
  test("builds a detached MultiSignature and round-trips through bytes", () => {
    const ms = new SignatureBuilder("eddsa-msig").withSignature(SIG_64).tryBuild();
    expect(ms.codec).toBe("eddsa-msig");
    expect(ms.isCombined).toBe(false);
    expect(ms.signatureBytes().bytes).toEqual(SIG_64);
    expect(ms.message.length).toBe(0);

    const decoded = new MultiSignature(ms.bytes);
    expect(decoded.codec).toBe("eddsa-msig");
    expect(decoded.signatureBytes().bytes).toEqual(SIG_64);
    expect(decoded.hex).toBe(ms.hex);
  });

  test("combined signature carries the message in-band", () => {
    const msg = Buffer.from("hello world");
    const ms = new SignatureBuilder("eddsa-msig")
      .withSignature(SIG_64)
      .withMessage(msg)
      .tryBuild();
    expect(ms.isCombined).toBe(true);
    expect(ms.message).toEqual(msg);
    expect(ms.signatureBytes().bytes).toEqual(SIG_64);

    // ...and the combined form survives a wire round-trip with the message intact.
    const decoded = new MultiSignature(ms.bytes);
    expect(decoded.isCombined).toBe(true);
    expect(decoded.message).toEqual(msg);
  });

  test("tryBuildEncoded returns a lowercase-hex string matching the binary form", () => {
    const hex = new SignatureBuilder("eddsa-msig").withSignature(SIG_64).tryBuildEncoded();
    expect(hex).toMatch(/^[0-9a-f]+$/);
    const ms = new SignatureBuilder("eddsa-msig").withSignature(SIG_64).tryBuild();
    expect(ms.hex).toBe(hex);
  });

  test("optional setters: payload encoding, scheme, threshold, limit, index, depth", () => {
    const ms = new SignatureBuilder("eddsa-msig")
      .withSignature(SIG_64)
      .withPayloadEncoding("sha2-256")
      .withScheme(2)
      .withThreshold(3)
      .withLimit(5)
      .withSigIndex(7)
      .withDepth(4)
      .tryBuild();
    expect(ms.codec).toBe("eddsa-msig");
    expect(ms.sigIndex).toBe(7);
    expect(ms.depth).toBe(4);
  });

  test("chained setters return the same instance", () => {
    const b = new SignatureBuilder("eddsa-msig");
    expect(b.withSignature(SIG_64)).toBe(b);
    expect(b.withMessage(Buffer.from("m"))).toBe(b);
  });

  test("rejects an unknown codec", () => {
    expect(() => new SignatureBuilder("nope-not-a-codec")).toThrow();
  });

  test("accepts an optional keyFormat exposed via the getter", () => {
    const ms = new SignatureBuilder("eddsa-msig").withSignature(SIG_64).tryBuild();
    const decoded = new Signature(ms.bytes, CryptoKey.Format.RAW);
    expect(decoded.keyFormat).toBe("raw");
  });
});

describe("threshold disclosure", () => {
  test("generateMetaKey returns 32 random bytes", () => {
    const k1 = SignatureBuilder.generateMetaKey();
    const k2 = SignatureBuilder.generateMetaKey();
    expect(k1.length).toBe(32);
    expect(k2.length).toBe(32);
    expect(k1.equals(k2)).toBe(false);
  });

  test("withDisclosure stamps a BLS G2 share (Partial mode)", () => {
    const metaKey = SignatureBuilder.generateMetaKey();
    const ms = new SignatureBuilder("bls12_381-g2-share-msig")
      .withDisclosure(1, metaKey, 3, 5)
      .withIdentifier(Buffer.alloc(32, 9))
      .withSignature(Buffer.alloc(96, 3))
      .tryBuild();
    expect(ms.codec).toBe("bls12_381-g2-share-msig");
    expect(ms.signatureBytes().bytes).toEqual(Buffer.alloc(96, 3));
  });

  test("withDisclosure Full mode needs no meta key", () => {
    const ms = new SignatureBuilder("bls12_381-g2-share-msig")
      .withDisclosure(0, null, 2, 3)
      .withIdentifier(Buffer.alloc(32, 1))
      .withSignature(Buffer.alloc(96, 3))
      .tryBuild();
    expect(ms.codec).toBe("bls12_381-g2-share-msig");
  });

  test("withDisclosure rejects an invalid mode", () => {
    expect(() =>
      new SignatureBuilder("bls12_381-g2-share-msig").withDisclosure(9, null, 3, 5),
    ).toThrow();
  });
});

describe("BLS constructors", () => {
  test("newFromBlsSignature rejects a non-BLS codec", () => {
    expect(() =>
      SignatureBuilder.newFromBlsSignature("eddsa-msig", Buffer.alloc(64)),
    ).toThrow();
  });

  test("newFromBlsSignatureShare rejects a non-share codec", () => {
    expect(() =>
      SignatureBuilder.newFromBlsSignatureShare("eddsa-msig", 3, 5, Buffer.alloc(96)),
    ).toThrow();
  });

  test("newFromBlsSignature rejects malformed BLS bytes", () => {
    expect(() =>
      SignatureBuilder.newFromBlsSignature("bls12_381-g2-msig", Buffer.alloc(96, 1)),
    ).toThrow();
  });
});

const enc = new TextEncoder();
const dec = new TextDecoder();
const crypto = webcrypto;

describe("CryptoKey — generation", () => {
  test("generates an AES secret key", () => {
    const key = new CryptoKey({ name: "AES-GCM", length: 256 }, true, ["encrypt", "decrypt"]);
    expect(key.type).toBe("secret");
    expect(key.extractable).toBe(true);
    expect(key.algorithm.name).toBe("AES-GCM");
    expect(key.algorithm.length).toBe(256);
    expect(key.usages).toContain("encrypt");
    expect(key.publicKey).toBeUndefined();
    expect(key.privateKey).toBeUndefined();
  });

  test("generates an HMAC secret key", () => {
    const key = new CryptoKey({ name: "HMAC", hash: { name: "SHA-256" } }, true, ["sign", "verify"]);
    expect(key.type).toBe("secret");
    expect(key.algorithm.hash?.name).toBe("SHA-256");
  });

  test("generates an EC key pair exposed via publicKey/privateKey", () => {
    const kp = new CryptoKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign", "verify"]);
    expect(kp.type).toBe("private");
    expect(kp.publicKey).toBeDefined();
    expect(kp.privateKey).toBeDefined();
    expect(kp.publicKey!.type).toBe("public");
    expect(kp.privateKey!.type).toBe("private");
  });

  test("generates Ed25519 / X25519 key pairs", () => {
    const ed = new CryptoKey({ name: "Ed25519" }, true, ["sign", "verify"]);
    expect(ed.publicKey).toBeDefined();
    const x = new CryptoKey({ name: "X25519" }, true, ["deriveKey", "deriveBits"]);
    expect(x.publicKey).toBeDefined();
  });

  test("generates an RSA key pair", () => {
    const rsa = new CryptoKey(
      { name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: { name: "SHA-256" } },
      true,
      ["sign", "verify"],
    );
    expect(rsa.publicKey).toBeDefined();
    expect(rsa.privateKey).toBeDefined();
  });
});

describe("CryptoKey — symmetric encrypt/decrypt", () => {
  test("AES-GCM roundtrip", () => {
    const key = new CryptoKey({ name: "AES-GCM", length: 256 }, true, ["encrypt", "decrypt"]);
    const iv = crypto.getRandomValues(new Uint8Array(12));
    const pt = enc.encode("hello world");
    const ct = CryptoKey.encrypt({ name: "AES-GCM", iv }, key, pt);
    expect(ct.length).toBeGreaterThan(pt.length);
    const back = CryptoKey.decrypt({ name: "AES-GCM", iv }, key, ct);
    expect(dec.decode(back)).toBe("hello world");
  });

  test("AES-CBC roundtrip", () => {
    const key = new CryptoKey({ name: "AES-CBC", length: 256 }, true, ["encrypt", "decrypt"]);
    const iv = crypto.getRandomValues(new Uint8Array(16));
    const pt = enc.encode("hello cbc");
    const ct = CryptoKey.encrypt({ name: "AES-CBC", iv }, key, pt);
    const back = CryptoKey.decrypt({ name: "AES-CBC", iv }, key, ct);
    expect(dec.decode(back)).toBe("hello cbc");
  });
});

describe("CryptoKey — sign/verify", () => {
  test("HMAC sign/verify", () => {
    const key = new CryptoKey({ name: "HMAC", hash: { name: "SHA-256" } }, true, ["sign", "verify"]);
    const data = enc.encode("message");
    const sig = CryptoKey.sign({ name: "HMAC", hash: { name: "SHA-256" } }, key, data);
    expect(CryptoKey.verify({ name: "HMAC", hash: { name: "SHA-256" } }, key, sig, data)).toBe(true);
    const tampered = enc.encode("tampered");
    expect(CryptoKey.verify({ name: "HMAC", hash: { name: "SHA-256" } }, key, sig, tampered)).toBe(false);
  });

  test("ECDSA sign/verify on a key pair", () => {
    const kp = new CryptoKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign", "verify"]);
    const data = enc.encode("ecdsa");
    const sig = CryptoKey.sign({ name: "ECDSA", hash: { name: "SHA-256" } }, kp.privateKey!, data);
    expect(CryptoKey.verify({ name: "ECDSA", hash: { name: "SHA-256" } }, kp.publicKey!, sig, data)).toBe(true);
  });

  test("Ed25519 sign/verify", () => {
    const kp = new CryptoKey({ name: "Ed25519" }, true, ["sign", "verify"]);
    const data = enc.encode("ed25519");
    const sig = CryptoKey.sign({ name: "Ed25519" }, kp.privateKey!, data);
    expect(CryptoKey.verify({ name: "Ed25519" }, kp.publicKey!, sig, data)).toBe(true);
  });

  test("RSA sign/verify", () => {
    const kp = new CryptoKey(
      { name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: { name: "SHA-256" } },
      true,
      ["sign", "verify"],
    );
    const data = enc.encode("rsa");
    const sig = CryptoKey.sign({ name: "RSASSA-PKCS1-v1_5", hash: { name: "SHA-256" } }, kp.privateKey!, data);
    expect(CryptoKey.verify({ name: "RSASSA-PKCS1-v1_5", hash: { name: "SHA-256" } }, kp.publicKey!, sig, data)).toBe(true);
  });
});

describe("CryptoKey — export/import", () => {
  test("HMAC JWK roundtrip", () => {
    const key = new CryptoKey({ name: "HMAC", hash: { name: "SHA-512" } }, true, ["sign", "verify"]);
    const jwk = CryptoKey.exportKey("jwk", key);
    expect((jwk as any).kty).toBe("oct");
    const imported = CryptoKey.importKey("jwk", jwk, { name: "HMAC", hash: { name: "SHA-512" } }, true, ["sign", "verify"]);
    const data = enc.encode("x");
    const sig = CryptoKey.sign({ name: "HMAC", hash: { name: "SHA-512" } }, key, data);
    expect(CryptoKey.verify({ name: "HMAC", hash: { name: "SHA-512" } }, imported, sig, data)).toBe(true);
  });

  test("AES raw roundtrip", () => {
    const key = new CryptoKey({ name: "AES-GCM", length: 128 }, true, ["encrypt", "decrypt"]);
    const raw = CryptoKey.exportKey("raw", key);
    expect(raw.length).toBe(16);
    const imported = CryptoKey.importKey("raw", raw, { name: "AES-GCM", length: 128 }, true, ["encrypt", "decrypt"]);
    expect(imported.type).toBe("secret");
  });

  test("EC pkcs8/spki roundtrip", () => {
    const kp = new CryptoKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign", "verify"]);
    const pkcs8 = CryptoKey.exportKey("pkcs8", kp.privateKey!);
    const spki = CryptoKey.exportKey("spki", kp.publicKey!);
    const priv = CryptoKey.importKey("pkcs8", pkcs8, { name: "ECDSA", namedCurve: "P-256" }, true, ["sign", "verify"]);
    const pub = CryptoKey.importKey("spki", spki, { name: "ECDSA", namedCurve: "P-256" }, true, ["verify"]);
    const data = enc.encode("der");
    const sig = CryptoKey.sign({ name: "ECDSA", hash: { name: "SHA-256" } }, priv, data);
    expect(CryptoKey.verify({ name: "ECDSA", hash: { name: "SHA-256" } }, pub, sig, data)).toBe(true);
  });

  test("non-extractable key cannot be exported", () => {
    const key = new CryptoKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
    expect(() => CryptoKey.exportKey("raw", key)).toThrow();
  });
});

describe("CryptoKey — digest", () => {
  test("SHA-256 produces 32 bytes", () => {
    const digest = CryptoKey.digest({ name: "SHA-256" }, enc.encode("abc"));
    expect(digest.length).toBe(32);
  });
});

describe("CryptoKey — deriveKey (PBKDF2)", () => {
  test("derives an AES key from a password", () => {
    const pw = CryptoKey.importKey("raw", enc.encode("password"), { name: "PBKDF2" }, false, ["deriveKey"]);
    const derived = CryptoKey.deriveKey(
      { name: "PBKDF2", salt: crypto.getRandomValues(new Uint8Array(16)), iterations: 100_000, hash: { name: "SHA-256" } },
      pw,
      { name: "AES-GCM", length: 256 },
      false,
      ["encrypt", "decrypt"],
    );
    expect(derived.type).toBe("secret");
    expect(derived.algorithm.name).toBe("AES-GCM");
  });
});

describe("CryptoKey — wrap/unwrap", () => {
  test("AES-KW wrap/unwrap roundtrip of an HMAC key (jwk)", () => {
    const kw = new CryptoKey({ name: "AES-KW", length: 256 }, true, ["wrapKey", "unwrapKey"]);
    const hmac = new CryptoKey({ name: "HMAC", hash: { name: "SHA-512" } }, true, ["sign", "verify"]);
    const wrapped = CryptoKey.wrapKey("jwk", hmac, kw, "AES-KW");
    const unwrapped = CryptoKey.unwrapKey(
      "jwk",
      wrapped,
      kw,
      "AES-KW",
      { name: "HMAC", hash: { name: "SHA-512" } },
      true,
      ["sign", "verify"],
    );
    const data = enc.encode("wrap");
    const sig = CryptoKey.sign({ name: "HMAC", hash: { name: "SHA-512" } }, hmac, data);
    expect(CryptoKey.verify({ name: "HMAC", hash: { name: "SHA-512" } }, unwrapped, sig, data)).toBe(true);
  });
});

describe("CryptoKey — supports / Format", () => {
  test("supports reflects implemented operations", () => {
    expect(CryptoKey.supports("encrypt", "AES-GCM")).toBe(true);
    expect(CryptoKey.supports("sign", "Ed25519")).toBe(true);
    expect(CryptoKey.supports("deriveBits", "ECDH")).toBe(true);
    expect(CryptoKey.supports("encapsulateBits", "ML-KEM-768")).toBe(false);
  });

  test("Format exposes key-format identifiers", () => {
    expect(CryptoKey.Format.JWK).toBe("jwk");
    expect(CryptoKey.Format.SPKI).toBe("spki");
    expect(CryptoKey.Format.PKCS8).toBe("pkcs8");
    expect(CryptoKey.Format.RAW).toBe("raw");
  });
});

describe("CryptoKey — instance method parity", () => {
  test("instance encrypt/decrypt mirrors the static API", () => {
    const key = new CryptoKey({ name: "AES-GCM", length: 256 }, true, ["encrypt", "decrypt"]);
    const iv = crypto.getRandomValues(new Uint8Array(12));
    const ct = key.encrypt({ name: "AES-GCM", iv }, key, enc.encode("instance"));
    const back = key.decrypt({ name: "AES-GCM", iv }, key, ct);
    expect(dec.decode(back)).toBe("instance");
  });

  test("instance deriveKey mirrors the static API", () => {
    const pw = CryptoKey.importKey("raw", enc.encode("password"), { name: "PBKDF2" }, false, ["deriveKey"]);
    const derived = pw.deriveKey(
      { name: "PBKDF2", salt: crypto.getRandomValues(new Uint8Array(16)), iterations: 100_000, hash: { name: "SHA-256" } },
      pw,
      { name: "AES-GCM", length: 256 },
      false,
      ["encrypt", "decrypt"],
    );
    expect(derived.algorithm.name).toBe("AES-GCM");
  });

  test("instance wrapKey/unwrapKey mirrors the static API", () => {
    const kw = new CryptoKey({ name: "AES-KW", length: 256 }, true, ["wrapKey", "unwrapKey"]);
    const hmac = new CryptoKey({ name: "HMAC", hash: { name: "SHA-512" } }, true, ["sign", "verify"]);
    const wrapped = kw.wrapKey("jwk", hmac, kw, "AES-KW");
    const unwrapped = kw.unwrapKey(
      "jwk",
      wrapped,
      kw,
      "AES-KW",
      { name: "HMAC", hash: { name: "SHA-512" } },
      true,
      ["sign", "verify"],
    );
    const data = enc.encode("wrap-instance");
    const sig = CryptoKey.sign({ name: "HMAC", hash: { name: "SHA-512" } }, hmac, data);
    expect(CryptoKey.verify({ name: "HMAC", hash: { name: "SHA-512" } }, unwrapped, sig, data)).toBe(true);
  });

  test("instance digest mirrors the static API", () => {
    const key = new CryptoKey({ name: "AES-GCM", length: 256 }, true, ["encrypt", "decrypt"]);
    const d = key.digest({ name: "SHA-256" }, enc.encode("abc"));
    expect(d.length).toBe(32);
  });

  test("KEM instance methods are present but not implemented", () => {
    const x = new CryptoKey({ name: "X25519" }, true, ["deriveKey", "deriveBits"]);
    expect(() => x.encapsulateBits({ name: "ML-KEM-768" }, x.publicKey!)).toThrow();
    expect(() => x.decapsulateBits({ name: "ML-KEM-768" }, x.privateKey!, new Uint8Array(32))).toThrow();
  });
});

describe("KeyFormat / key_formats", () => {
  test("allKeyFormats lists every supported format", () => {
    const ids = allKeyFormats().sort();
    expect(ids).toEqual(
      ["jwk", "pkcs8", "raw", "raw-public", "raw-secret", "raw-seed", "spki"].sort(),
    );
  });

  test("keyFormats(algorithm) reports import/export sets", () => {
    const jwk = keyFormats("AES-GCM")!;
    expect(jwk.import).toContain("jwk");
    expect(jwk.export).toContain("jwk");

    const ec = keyFormats("ECDSA")!;
    expect(ec.import).toContain("spki");
    expect(ec.import).toContain("pkcs8");

    const argon = keyFormats("Argon2id")!;
    expect(argon.import).toContain("raw-secret");
    expect(argon.export).toHaveLength(0);

    expect(keyFormats("Does-Not-Exist")).toBeUndefined();
  });

  test("CryptoKey.Format exposes key-format identifiers", () => {
    expect(CryptoKey.Format.JWK).toBe("jwk");
    expect(CryptoKey.Format.SPKI).toBe("spki");
    expect(CryptoKey.Format.PKCS8).toBe("pkcs8");
    expect(CryptoKey.Format.RAW).toBe("raw");
    expect(CryptoKey.Format.RAW_SECRET).toBe("raw-secret");
    expect(CryptoKey.Format.RAW_PUBLIC).toBe("raw-public");
    expect(CryptoKey.Format.RAW_SEED).toBe("raw-seed");
  });
});

describe("Signature (format-aware)", () => {
  test("construction tags the format and accepts bytes", () => {
    const sig = new Signature(CryptoKey.Format.RAW);
    expect(sig.format).toBe("raw");
    expect(sig.length).toBe(0);
    expect(sig.isEmpty).toBe(true);
    sig.setBytes(Buffer.from([9, 8, 7]));
    expect(sig.bytes).toEqual(Buffer.from([9, 8, 7]));
    expect(sig.length).toBe(3);
    expect(sig.isEmpty).toBe(false);
    expect(sig.hex).toBe("090807");
  });

  test("hex is lowercase", () => {
    const sig = new Signature(CryptoKey.Format.JWK);
    sig.setBytes(Buffer.from([0xff, 0x00, 0xab]));
    expect(sig.hex).toBe("ff00ab");
  });

  test("format defaults to the provided value and round-trips", () => {
    const sig = new Signature(CryptoKey.Format.SPKI);
    expect(sig.format).toBe("spki");
    sig.setBytes(SIG_64);
    expect(sig.bytes).toEqual(SIG_64);
  });
});
