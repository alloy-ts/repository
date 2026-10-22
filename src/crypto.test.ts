import { describe, expect, test } from "vite-plus/test";
import { CryptoKey, Signature, SignatureBuilder, SignatureScheme } from "./main.ts";

const SIG_64 = Buffer.alloc(64, 7);

describe("SignatureScheme", () => {
  test("from a codec name", () => {
    const s = SignatureScheme.fromCodec("eddsa-msig");
    expect(s.name).toBe("eddsa-msig");
    expect(typeof s.code).toBe("number");
  });

  test("from a numeric codec round-trips with the name", () => {
    const byName = SignatureScheme.fromCodec("eddsa-msig");
    const byCode = SignatureScheme.fromCodec(byName.code);
    expect(byCode.name).toBe("codec-1");
    expect(byCode.code).toBe(byName.code);
  });

  test("rejects an unknown codec", () => {
    expect(() => SignatureScheme.fromCodec("not-a-real-codec")).toThrow();
  });
});

describe("SignatureBuilder / Signature", () => {
  test("builds a detached Signature and round-trips through bytes", () => {
    const ms = new SignatureBuilder("eddsa-msig").withSignature(SIG_64).tryBuild();
    expect(ms.signatureBytes().bytes).toEqual(SIG_64);

    const decoded = new Signature(ms.bytes);
    expect(decoded.signatureBytes().bytes).toEqual(SIG_64);
    expect(decoded.hex).toBe(ms.hex);
  });

  test("combined signature carries the message in-band", () => {
    const msg = Buffer.from("hello world");
    const ms = new SignatureBuilder("eddsa-msig")
      .withSignature(SIG_64)
      .withMessage(msg)
      .tryBuild();
    expect(ms.signatureBytes().bytes).toEqual(SIG_64);

    const decoded = new Signature(ms.bytes);
    expect(decoded.asBytes()).toEqual(SIG_64);
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
    expect(ms.length).toBe(64);
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

describe("CryptoKey", () => {
  test("AES key generation and usage", () => {
    const key = new CryptoKey(
      {
        name: "AES-CBC",
        length: 256,
      },
      true,
      ["encrypt", "decrypt"],
    );

    expect(key.type).toBe("secret");
    expect(key.extractable).toBe(true);
    expect(key.algorithm.name).toBe("AES-CBC");
    expect(key.usages).toEqual(["encrypt", "decrypt"]);
  });

  test("ECDSA key pair generation", () => {
    const keyPair = new CryptoKey(
      {
        name: "ECDSA",
        namedCurve: "P-521",
      },
      true,
      ["sign", "verify"],
    );

    expect(keyPair.publicKey).toBeDefined();
    expect(keyPair.privateKey).toBeDefined();
    expect(keyPair.publicKey?.type).toBe("public");
    expect(keyPair.privateKey?.type).toBe("private");
    expect(keyPair.algorithm.name).toBe("ECDSA");
  });

  test("Ed25519 / X25519 key generation", () => {
    const edKey = new CryptoKey({ name: "Ed25519" }, true, ["sign", "verify"]);
    expect(edKey.algorithm.name).toBe("Ed25519");

    const xKey = new CryptoKey({ name: "X25519" }, true, ["deriveKey"]);
    expect(xKey.algorithm.name).toBe("X25519");
  });

  test("HMAC key generation", () => {
    const key = new CryptoKey(
      {
        name: "HMAC",
        hash: "SHA-256",
      },
      true,
      ["sign", "verify"],
    );

    expect(key.type).toBe("secret");
    expect(key.algorithm.name).toBe("HMAC");
  });

  test("RSA key pair generation", () => {
    const publicExponent = Buffer.from([1, 0, 1]);
    const keyPair = new CryptoKey(
      {
        name: "RSASSA-PKCS1-v1_5",
        modulusLength: 2048,
        publicExponent,
        hash: "SHA-256",
      },
      true,
      ["sign", "verify"],
    );

    expect(keyPair.publicKey).toBeDefined();
    expect(keyPair.privateKey).toBeDefined();
    expect(keyPair.algorithm.name).toBe("RSASSA-PKCS1-v1_5");
  });

  test("encryption and decryption with AES-CBC", () => {
    const key = new CryptoKey({ name: "AES-CBC", length: 256 }, true, ["encrypt", "decrypt"]);
    const iv = Buffer.alloc(16, 1);
    const plaintext = Buffer.from("Hello Web Crypto API!");

    const ciphertext = key.encrypt({ name: "AES-CBC", iv }, key, plaintext);
    expect(ciphertext).toBeDefined();
    expect(ciphertext).not.toEqual(plaintext);

    const decrypted = key.decrypt({ name: "AES-CBC", iv }, key, ciphertext);
    expect(decrypted).toEqual(plaintext);
  });

  test("exporting and importing keys", () => {
    const key = new CryptoKey(
      { name: "HMAC", hash: "SHA-512" },
      true,
      ["sign", "verify"],
    );

    const exported = CryptoKey.exportKey("jwk", key);
    expect(exported).toBeDefined();

    const imported = CryptoKey.import(
      "jwk",
      exported,
      { name: "HMAC", hash: "SHA-512" },
      true,
      ["sign", "verify"],
    );
    expect(imported.algorithm.name).toBe("HMAC");
  });

  test("wrapping and unwrapping keys", () => {
    const key = new CryptoKey({ name: "HMAC", hash: "SHA-512" }, true, ["sign", "verify"]);
    const wrappingKey = new CryptoKey({ name: "AES-KW", length: 256 }, true, [
      "wrapKey",
      "unwrapKey",
    ]);

    const wrappedKey = CryptoKey.wrapKey("jwk", key, wrappingKey, "AES-KW");
    expect(wrappedKey).toBeDefined();

    const unwrappedKey = CryptoKey.unwrapKey(
      "jwk",
      wrappedKey,
      wrappingKey,
      "AES-KW",
      { name: "HMAC", hash: "SHA-512" },
      true,
      ["sign", "verify"],
    );
    expect(unwrappedKey.algorithm.name).toBe("HMAC");
  });

  test("CryptoKey.supports feature detection", () => {
    expect(CryptoKey.supports("importKey", "AES-GCM")).toBe(true);
    expect(CryptoKey.supports("sign", "ECDSA")).toBe(true);
    expect(CryptoKey.supports("encrypt", "NON_EXISTENT")).toBe(false);
  });

  test("CryptoKey.Format and Signature with CryptoKeyFormat", () => {
    const fmt = { format: "jwk" };
    const sig = new Signature(fmt);
    expect(sig.format).toEqual(fmt);
  });
});
