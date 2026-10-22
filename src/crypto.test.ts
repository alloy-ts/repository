import { describe, expect, test } from "vite-plus/test";
import { CryptoKey, KeyFormat, Signature, isFormatSupported } from "./main.ts";

describe("CryptoKey & format NAPI bindings", () => {
  test("AES key construction via new CryptoKey", () => {
    const key = new CryptoKey(
      {
        name: "AES-CBC",
        length: 256,
      },
      true,
      ["encrypt", "decrypt"]
    );
    expect(key).toBeDefined();
    expect(key.type).toBe("secret");
  });

  test("ECDSA key pair destructuring ({ publicKey, privateKey })", () => {
    const key = new CryptoKey(
      {
        name: "ECDSA",
        namedCurve: "P-521",
      },
      true,
      ["sign", "verify"]
    );
    const { publicKey, privateKey } = key;
    expect(publicKey).toBeDefined();
    expect(privateKey).toBeDefined();
    expect(publicKey.type).toBe("public");
    expect(privateKey.type).toBe("private");
  });

  test("Ed25519 & X25519 key construction", () => {
    const edKey = new CryptoKey({ name: "Ed25519" }, true, ["sign", "verify"]);
    const xKey = new CryptoKey({ name: "X25519" }, true, ["deriveKey"]);
    expect(edKey.type).toBe("private");
    expect(xKey.type).toBe("private");
  });

  test("CryptoKey.supports feature detection", () => {
    expect(CryptoKey.supports("encrypt", "AES-CBC")).toBe(true);
    expect(CryptoKey.supports("sign", "HMAC")).toBe(true);
    expect(CryptoKey.supports("decapsulateKey", "ML-KEM-768")).toBe(true);
    expect(CryptoKey.supports("encrypt", "UNSUPPORTED_ALG")).toBe(false);
  });

  test("Signature constructor with KeyFormat", () => {
    const sig = new Signature(KeyFormat.Raw);
    expect(sig).toBeDefined();
    expect(sig.keyFormat).toBe(KeyFormat.Raw);
  });

  test("isFormatSupported queries key format support", () => {
    expect(isFormatSupported("jwk", "AES-CBC", false)).toBe(true);
    expect(isFormatSupported("spki", "ECDSA", false)).toBe(true);
    expect(isFormatSupported("spki", "HMAC", false)).toBe(false);
  });
});
