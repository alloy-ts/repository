import { describe, expect, test } from "vite-plus/test";
import {
  CryptoKeyFormat,
  Multisig,
  MultisigBuilder,
  Signature,
  SignatureBytes,
  SignatureScheme,
  createCryptoKey,
  createCryptoKeyPair,
  cryptoDeriveBits,
  cryptoDeriveKey,
  cryptoGetPublicKey,
  cryptoSign,
  cryptoSupports,
  cryptoVerify,
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

describe("MultisigBuilder / Multisig", () => {
  test("builds a detached Multisig and round-trips through bytes", () => {
    const ms = new MultisigBuilder("eddsa-msig").withSignature(SIG_64).tryBuild();
    expect(ms.codec).toBe("eddsa-msig");
    expect(ms.isCombined).toBe(false);
    expect(ms.signatureBytes().bytes).toEqual(SIG_64);
    expect(ms.message.length).toBe(0);

    const decoded = new Multisig(ms.bytes);
    expect(decoded.codec).toBe("eddsa-msig");
    expect(decoded.signatureBytes().bytes).toEqual(SIG_64);
    expect(decoded.hex).toBe(ms.hex);
  });

  test("combined signature carries the message in-band", () => {
    const msg = Buffer.from("hello world");
    const ms = new MultisigBuilder("eddsa-msig")
      .withSignature(SIG_64)
      .withMessage(msg)
      .tryBuild();
    expect(ms.isCombined).toBe(true);
    expect(ms.message).toEqual(msg);
    expect(ms.signatureBytes().bytes).toEqual(SIG_64);

    // ...and the combined form survives a wire round-trip with the message intact.
    const decoded = new Multisig(ms.bytes);
    expect(decoded.isCombined).toBe(true);
    expect(decoded.message).toEqual(msg);
  });

  test("tryBuildEncoded returns a lowercase-hex string matching the binary form", () => {
    const hex = new MultisigBuilder("eddsa-msig").withSignature(SIG_64).tryBuildEncoded();
    expect(hex).toMatch(/^[0-9a-f]+$/);
    const ms = new MultisigBuilder("eddsa-msig").withSignature(SIG_64).tryBuild();
    expect(ms.hex).toBe(hex);
  });

  test("optional setters: payload encoding, scheme, threshold, limit, index, depth", () => {
    const ms = new MultisigBuilder("eddsa-msig")
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
    const b = new MultisigBuilder("eddsa-msig");
    expect(b.withSignature(SIG_64)).toBe(b);
    expect(b.withMessage(Buffer.from("m"))).toBe(b);
  });

  test("rejects an unknown codec", () => {
    expect(() => new MultisigBuilder("nope-not-a-codec")).toThrow();
  });
});

describe("threshold disclosure", () => {
  test("generateMetaKey returns 32 random bytes", () => {
    const k1 = MultisigBuilder.generateMetaKey();
    const k2 = MultisigBuilder.generateMetaKey();
    expect(k1.length).toBe(32);
    expect(k2.length).toBe(32);
    expect(k1.equals(k2)).toBe(false);
  });

  test("withDisclosure stamps a BLS G2 share (Partial mode)", () => {
    const metaKey = MultisigBuilder.generateMetaKey();
    const ms = new MultisigBuilder("bls12_381-g2-share-msig")
      .withDisclosure(1, metaKey, 3, 5)
      .withIdentifier(Buffer.alloc(32, 9))
      .withSignature(Buffer.alloc(96, 3))
      .tryBuild();
    expect(ms.codec).toBe("bls12_381-g2-share-msig");
    expect(ms.signatureBytes().bytes).toEqual(Buffer.alloc(96, 3));
  });

  test("withDisclosure Full mode needs no meta key", () => {
    const ms = new MultisigBuilder("bls12_381-g2-share-msig")
      .withDisclosure(0, null, 2, 3)
      .withIdentifier(Buffer.alloc(32, 1))
      .withSignature(Buffer.alloc(96, 3))
      .tryBuild();
    expect(ms.codec).toBe("bls12_381-g2-share-msig");
  });

  test("withDisclosure rejects an invalid mode", () => {
    expect(() =>
      new MultisigBuilder("bls12_381-g2-share-msig").withDisclosure(9, null, 3, 5),
    ).toThrow();
  });
});

describe("CryptoKey / CryptoKeyPair NAPI object bindings", () => {
  test("AES key object structure created via NAPI binding", () => {
    const key = createCryptoKey(
      {
        name: "AES-CBC",
        length: 256,
      },
      true,
      "secret",
      ["encrypt", "decrypt"],
    );

    expect(key.algorithm.name).toBe("AES-CBC");
    expect((key.algorithm as any).length).toBe(256);
    expect(key.extractable).toBe(true);
    expect(key.type).toBe("secret");
    expect(key.usages).toEqual(["encrypt", "decrypt"]);
  });

  test("ECDSA key pair object structure created via NAPI binding", () => {
    const pub = createCryptoKey(
      {
        name: "ECDSA",
        namedCurve: "P-521",
      },
      true,
      "public",
      ["verify"],
    );

    const priv = createCryptoKey(
      {
        name: "ECDSA",
        namedCurve: "P-521",
      },
      false,
      "private",
      ["sign"],
    );

    const pair = createCryptoKeyPair(pub, priv);

    expect(pair.publicKey.algorithm.name).toBe("ECDSA");
    expect((pair.publicKey.algorithm as any).namedCurve).toBe("P-521");
    expect(pair.publicKey.type).toBe("public");
    expect(pair.privateKey.type).toBe("private");
    expect(pair.privateKey.usages).toEqual(["sign"]);
  });

  test("HMAC key object structure created via NAPI binding", () => {
    const key = createCryptoKey(
      {
        name: "HMAC",
        length: 256,
        hash: { name: "SHA-256" },
      },
      true,
      "secret",
      ["sign", "verify"],
    );

    expect(key.algorithm.name).toBe("HMAC");
    expect((key.algorithm as any).hash?.name).toBe("SHA-256");
    expect(key.usages).toEqual(["sign", "verify"]);
  });

  test("RSA key pair object structure created via NAPI binding", () => {
    const publicExponent = Buffer.from([1, 0, 1]);
    const pub = createCryptoKey(
      {
        name: "RSASSA-PKCS1-v1_5",
        modulusLength: 2048,
        publicExponent,
        hash: { name: "SHA-256" },
      },
      true,
      "public",
      ["verify"],
    );

    const priv = createCryptoKey(
      {
        name: "RSASSA-PKCS1-v1_5",
        modulusLength: 2048,
        publicExponent,
        hash: { name: "SHA-256" },
      },
      false,
      "private",
      ["sign"],
    );

    const pair = createCryptoKeyPair(pub, priv);

    expect((pair.publicKey.algorithm as any).modulusLength).toBe(2048);
    expect(Buffer.from((pair.publicKey.algorithm as any).publicExponent)).toEqual(
      Buffer.from([1, 0, 1]),
    );
    expect(pair.publicKey.type).toBe("public");
    expect(pair.privateKey.type).toBe("private");
  });

  test("Signature constructor accepts string and CryptoKeyFormat enum", () => {
    const s1 = new Signature("jwk");
    expect(s1.format).toBe("jwk");

    const s2 = new Signature(CryptoKeyFormat.Spki);
    expect(s2.format).toBe("spki");

    const s3 = new Signature(CryptoKeyFormat.Pkcs8);
    expect(s3.format).toBe("pkcs8");
  });

  test("cryptoSign and cryptoVerify functions", () => {
    const key = createCryptoKey(
      { name: "HMAC", length: 256, hash: { name: "SHA-256" } },
      true,
      "secret",
      ["sign", "verify"],
    );
    const data = Buffer.from("hello signature");
    const sig = cryptoSign("HMAC", key, data);
    expect(sig.length).toBeGreaterThan(data.length);
    expect(cryptoVerify("HMAC", key, sig, data)).toBe(true);
  });

  test("cryptoDeriveBits and cryptoDeriveKey functions", () => {
    const baseKey = createCryptoKey(
      { name: "PBKDF2" },
      false,
      "secret",
      ["deriveKey", "deriveBits"],
    );
    const bits = cryptoDeriveBits("PBKDF2", baseKey, 256);
    expect(bits.length).toBe(32);

    const derived = cryptoDeriveKey(
      "PBKDF2",
      baseKey,
      { name: "AES-GCM", length: 256 },
      true,
      ["encrypt", "decrypt"],
    );
    expect(derived.algorithm.name).toBe("AES-GCM");
    expect(derived.extractable).toBe(true);
  });

  test("cryptoGetPublicKey and cryptoSupports functions", () => {
    const pub = createCryptoKey(
      { name: "ECDSA", namedCurve: "P-256" },
      true,
      "public",
      ["verify"],
    );
    const priv = createCryptoKey(
      { name: "ECDSA", namedCurve: "P-256" },
      false,
      "private",
      ["sign"],
    );
    const pair = createCryptoKeyPair(pub, priv);
    const gotPub = cryptoGetPublicKey(pair);
    expect(gotPub.type).toBe("public");

    expect(cryptoSupports("encrypt", "AES-GCM")).toBe(true);
    expect(cryptoSupports("sign", "ECDSA")).toBe(true);
  });
});

describe("BLS constructors", () => {
  test("newFromBlsSignature rejects a non-BLS codec", () => {
    expect(() =>
      MultisigBuilder.newFromBlsSignature("eddsa-msig", Buffer.alloc(64)),
    ).toThrow();
  });

  test("newFromBlsSignatureShare rejects a non-share codec", () => {
    expect(() =>
      MultisigBuilder.newFromBlsSignatureShare("eddsa-msig", 3, 5, Buffer.alloc(96)),
    ).toThrow();
  });

  test("newFromBlsSignature rejects malformed BLS bytes", () => {
    expect(() =>
      MultisigBuilder.newFromBlsSignature("bls12_381-g2-msig", Buffer.alloc(96, 1)),
    ).toThrow();
  });
});
