export * from "../dist/index.js";

import * as native from "../dist/index.js";

export const CryptoKey = native.CryptoKey;
(CryptoKey as any).Format = { format: undefined };
(CryptoKey as any).exportKey = (format: string, key: any) => key.exportKey(format);
(CryptoKey as any).encrypt = (alg: any, key: any, data: any) => key.encrypt(alg, data);
(CryptoKey as any).decrypt = (alg: any, key: any, data: any) => key.decrypt(alg, data);
(CryptoKey as any).sign = (alg: any, key: any, data: any) => key.sign(alg, data);
(CryptoKey as any).verify = (alg: any, key: any, sig: any, data: any) => key.verify(alg, sig, data);
(CryptoKey as any).deriveBits = (alg: any, baseKey: any, length?: number) =>
  baseKey.deriveBits(alg, length);
(CryptoKey as any).deriveKey = (
  alg: any,
  baseKey: any,
  derivedKeyType: any,
  extractable: boolean,
  keyUsages: string[],
) => baseKey.deriveKey(alg, derivedKeyType, extractable, keyUsages);
(CryptoKey as any).wrapKey = (format: string, key: any, wrappingKey: any, wrapAlgorithm: any) =>
  key.wrapKey(format, wrappingKey, wrapAlgorithm);
(CryptoKey as any).unwrapKey = (
  format: string,
  wrappedKey: any,
  unwrappingKey: any,
  unwrapAlg: any,
  unwrappedAlg: any,
  ext: boolean,
  usages: string[],
) =>
  unwrappingKey.unwrapKey(format, wrappedKey, unwrappingKey, unwrapAlg, unwrappedAlg, ext, usages);
(CryptoKey as any).getPublicKey = (key: any) => key.publicKey ?? key;
