import { describe, expect, test } from "vite-plus/test";
import { CryptoKey, KeyFormat, Signature, isFormatSupported } from "./main.ts";

describe("CryptoKey & format NAPI bindings", () => {
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
