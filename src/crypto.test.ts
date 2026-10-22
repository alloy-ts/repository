import { describe, expect, test } from "vite-plus/test";
import type { CryptoKey, CryptoKeyPair } from "./main.ts";
import { Signature, SignatureBuilder, SignatureBytes, SignatureScheme } from "./main.ts";

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
    const ms = new SignatureBuilder("eddsa-msig")
      .withSignature(SIG_64)
      .withMessage(msg)
      .tryBuild();
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

describe("CryptoKey / CryptoKeyPair", () => {
  test("creates a symmetric CryptoKey object structure", () => {
    const key: CryptoKey = {
      algorithm: { name: "AES-CBC", length: 256 },
      extractable: true,
      type: "secret",
      usages: ["encrypt", "decrypt"],
    };

    expect(key.algorithm).toEqual({ name: "AES-CBC", length: 256 });
    expect(key.extractable).toBe(true);
    expect(key.type).toBe("secret");
    expect(key.usages).toEqual(["encrypt", "decrypt"]);
  });

  test("creates an asymmetric CryptoKeyPair object structure", () => {
    const publicKey: CryptoKey = {
      algorithm: { name: "ECDSA", namedCurve: "P-521" },
      extractable: true,
      type: "public",
      usages: ["verify"],
    };

    const privateKey: CryptoKey = {
      algorithm: { name: "ECDSA", namedCurve: "P-521" },
      extractable: false,
      type: "private",
      usages: ["sign"],
    };

    const pair: CryptoKeyPair = {
      publicKey,
      privateKey,
    };

    expect(pair.publicKey.type).toBe("public");
    expect(pair.privateKey.type).toBe("private");
    expect(pair.publicKey.usages).toEqual(["verify"]);
    expect(pair.privateKey.usages).toEqual(["sign"]);
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
