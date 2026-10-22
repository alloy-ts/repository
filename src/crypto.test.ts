import { describe, expect, test } from "vite-plus/test";
import { CryptoKey, Signature, SignatureBuilder, SignatureBytes, SignatureScheme } from "./main.ts";

const SIG_64 = Buffer.alloc(64, 7);

describe("Signature constructor with CryptoKey.Format", () => {
  test("accepts CryptoKey.Format in constructor", () => {
    const sig = new Signature(CryptoKey.Format.Raw);
    expect(sig).toBeDefined();
    expect(sig.codec).toBeDefined();
  });
});

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

describe("SignatureBuilder / Signature", () => {
  test("builds a detached Signature and round-trips through bytes", () => {
    const ms = new SignatureBuilder("eddsa-msig").withSignature(SIG_64).tryBuild();
    expect(ms.codec).toBe("eddsa-msig");
    expect(ms.isCombined).toBe(false);
    expect(ms.signatureBytes().bytes).toEqual(SIG_64);
    expect(ms.message.length).toBe(0);

    const decoded = new Signature(ms.bytes);
    expect(decoded.codec).toBe("eddsa-msig");
    expect(decoded.signatureBytes().bytes).toEqual(SIG_64);
    expect(decoded.hex).toBe(ms.hex);
  });

  test("combined signature carries the message in-band", () => {
    const msg = Buffer.from("hello world");
    const ms = new SignatureBuilder("eddsa-msig").withSignature(SIG_64).withMessage(msg).tryBuild();
    expect(ms.isCombined).toBe(true);
    expect(ms.message).toEqual(msg);
    expect(ms.signatureBytes().bytes).toEqual(SIG_64);

    // ...and the combined form survives a wire round-trip with the message intact.
    const decoded = new Signature(ms.bytes);
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
    expect(() => SignatureBuilder.newFromBlsSignature("eddsa-msig", Buffer.alloc(64))).toThrow();
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

describe("CryptoKey - WebCrypto Bindings", () => {
  test("AES key generation", () => {
    const key = new CryptoKey({ name: "AES-CBC", length: 256 }, true, ["encrypt", "decrypt"]);
    expect(key.type).toBe("secret");
    expect(key.extractable).toBe(true);
    expect(key.algorithm.name).toBe("AES-CBC");
    expect(key.usages).toEqual(["encrypt", "decrypt"]);
  });

  test("ECDSA key pairs generation", () => {
    const { publicKey, privateKey } = new CryptoKey({ name: "ECDSA", namedCurve: "P-521" }, true, [
      "sign",
      "verify",
    ]);
    expect(publicKey.type).toBe("public");
    expect(privateKey.type).toBe("private");
    expect(publicKey.algorithm.name).toBe("ECDSA");
    expect(privateKey.algorithm.namedCurve).toBe("P-521");
  });

  test("Ed25519 & X25519 key pairs generation", () => {
    const edKey = new CryptoKey({ name: "Ed25519" }, true, ["sign", "verify"]);
    expect(edKey.publicKey.type).toBe("public");
    expect(edKey.privateKey.type).toBe("private");

    const xKey = new CryptoKey({ name: "X25519" }, true, ["deriveKey"]);
    expect(xKey.publicKey.type).toBe("public");
    expect(xKey.privateKey.type).toBe("private");
  });

  test("HMAC key generation", () => {
    const key = new CryptoKey({ name: "HMAC", hash: { name: "SHA-256" } }, true, [
      "sign",
      "verify",
    ]);
    expect(key.type).toBe("secret");
    expect(key.algorithm.name).toBe("HMAC");
  });

  test("RSA key pairs generation", () => {
    const publicExponent = new Uint8Array([1, 0, 1]);
    const { publicKey, privateKey } = new CryptoKey(
      {
        name: "RSASSA-PKCS1-v1_5",
        modulusLength: 2048,
        publicExponent: Array.from(publicExponent),
        hash: { name: "SHA-256" },
      },
      true,
      ["sign", "verify"],
    );
    expect(publicKey.type).toBe("public");
    expect(privateKey.type).toBe("private");
  });

  test("Encryption and Decryption (static and instance)", () => {
    const key = new CryptoKey({ name: "AES-CBC", length: 256 }, true, ["encrypt", "decrypt"]);
    const iv = Buffer.alloc(16, 1);
    const plaintext = Buffer.from("Hello, World!");

    const cipher = key.encrypt({ name: "AES-CBC", iv: Array.from(iv) }, key, plaintext);
    expect(cipher).toBeDefined();

    const decrypted = key.decrypt({ name: "AES-CBC", iv: Array.from(iv) }, key, cipher);
    expect(decrypted.toString()).toBe("Hello, World!");
  });

  test("Exporting and Importing keys", () => {
    const key = new CryptoKey({ name: "HMAC", hash: { name: "SHA-512" } }, true, [
      "sign",
      "verify",
    ]);
    const exported = CryptoKey.exportKey("raw", key);
    expect(exported).toBeDefined();

    const imported = CryptoKey.import("raw", exported, { name: "HMAC" }, true, ["sign", "verify"]);
    expect(imported.type).toBe("secret");
  });

  test("Wrapping and Unwrapping keys", () => {
    const key = new CryptoKey({ name: "HMAC" }, true, ["sign", "verify"]);
    const wrappingKey = new CryptoKey({ name: "AES-KW", length: 256 }, true, [
      "wrapKey",
      "unwrapKey",
    ]);

    const wrappedKey = CryptoKey.wrapKey("raw", key, wrappingKey, { name: "AES-KW" });
    expect(wrappedKey).toBeDefined();

    const unwrapped = CryptoKey.unwrapKey(
      "raw",
      wrappedKey,
      wrappingKey,
      { name: "AES-KW" },
      { name: "HMAC" },
      true,
      ["sign", "verify"],
    );
    expect(unwrapped.type).toBe("secret");
  });

  test("Deriving keys and bits", () => {
    const baseKey = new CryptoKey({ name: "PBKDF2" }, true, ["deriveKey", "deriveBits"]);
    const bits = CryptoKey.deriveBits({ name: "PBKDF2" }, baseKey, 256);
    expect(bits.length).toBe(32);

    const derived = CryptoKey.deriveKey(
      { name: "PBKDF2" },
      baseKey,
      { name: "AES-GCM", length: 256 },
      true,
      ["encrypt"],
    );
    expect(derived.type).toBe("secret");
  });

  test("Decapsulating key", () => {
    const decapsulationKey = new CryptoKey({ name: "ML-KEM-768" }, true, ["decapsulateKey"]);
    const ciphertext = Buffer.alloc(1088, 5);

    const decapsulatedKey = decapsulationKey.decapsulateKey(
      { name: "ML-KEM-768" },
      decapsulationKey,
      ciphertext,
      { name: "AES-GCM", length: 256 },
      true,
      ["encrypt", "decrypt"],
    );
    expect(decapsulatedKey.type).toBe("secret");
  });

  test("CryptoKey.supports feature detection", () => {
    expect(CryptoKey.supports("importKey", "Argon2id")).toBe(true);
    expect(CryptoKey.supports("encrypt", "AES-CBC")).toBe(true);
    expect(CryptoKey.supports("sign", "ECDSA")).toBe(true);
    expect(CryptoKey.supports("decapsulateKey", "ML-KEM-768")).toBe(true);
    expect(CryptoKey.supports("unknownOp", "AES-CBC")).toBe(false);
  });

  test("CryptoKey.Format enum property", () => {
    expect(CryptoKey.Format).toBeDefined();
    expect(CryptoKey.Format.Raw).toBe(0);
  });
});
