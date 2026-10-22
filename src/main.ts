import * as native from "../dist/index.js";

export * from "../dist/index.js";

function attachKeyMethods<T>(keyObj: T): T {
  if (!keyObj || typeof keyObj !== "object") return keyObj;

  if ("publicKey" in keyObj && "privateKey" in keyObj) {
    attachKeyMethods((keyObj as any).publicKey);
    attachKeyMethods((keyObj as any).privateKey);
    return keyObj;
  }

  const k = keyObj as any;
  k.encrypt = (algorithm: any, key: any, data: any) =>
    native.CryptoKey_encrypt(algorithm, key || k, data);
  k.decrypt = (algorithm: any, key: any, data: any) =>
    native.CryptoKey_decrypt(algorithm, key || k, data);
  k.sign = (algorithm: any, key: any, data: any) =>
    native.CryptoKey_sign(algorithm, key || k, data);
  k.verify = (algorithm: any, key: any, signature: any, data: any) =>
    native.CryptoKey_verify(algorithm, key || k, signature, data);
  k.decapsulateKey = (
    decapsulationAlgorithm: any,
    decapsulationKey: any,
    ciphertext: any,
    sharedKeyAlgorithm: any,
    extractable: boolean,
    keyUsages: string[],
  ) =>
    native.CryptoKey_decapsulateKey(
      decapsulationAlgorithm,
      decapsulationKey || k,
      ciphertext,
      sharedKeyAlgorithm,
      extractable,
      keyUsages,
    );

  return k;
}

const CryptoKeyImpl: any = function (
  this: any,
  algorithm: any,
  extractable?: boolean,
  usages?: string[],
) {
  const res = native.CryptoKey(algorithm, extractable, usages);
  return attachKeyMethods(res);
};

const FormatEnum = {
  Raw: 0,
  RawSecret: 1,
  RawPublic: 2,
  RawSeed: 3,
  Pkcs8: 4,
  Spki: 5,
  Jwk: 6,
};

CryptoKeyImpl.supports = native.CryptoKey_supports;
CryptoKeyImpl.exportKey = native.CryptoKey_exportKey;
CryptoKeyImpl.importKey = (
  format: any,
  keyData: any,
  algorithm: any,
  extractable: boolean,
  keyUsages: string[],
) => {
  const k = native.CryptoKey_importKey(format, keyData, algorithm, extractable, keyUsages);
  return attachKeyMethods(k);
};
CryptoKeyImpl.import = CryptoKeyImpl.importKey;
CryptoKeyImpl.encrypt = native.CryptoKey_encrypt;
CryptoKeyImpl.decrypt = native.CryptoKey_decrypt;
CryptoKeyImpl.sign = native.CryptoKey_sign;
CryptoKeyImpl.verify = native.CryptoKey_verify;
CryptoKeyImpl.wrapKey = native.CryptoKey_wrapKey;
CryptoKeyImpl.unwrapKey = (
  format: any,
  wrappedKey: any,
  wrappingKey: any,
  unwrapAlgorithm: any,
  unwrappedKeyAlgorithm: any,
  extractable: boolean,
  keyUsages: string[],
) => {
  const k = native.CryptoKey_unwrapKey(
    format,
    wrappedKey,
    wrappingKey,
    unwrapAlgorithm,
    unwrappedKeyAlgorithm,
    extractable,
    keyUsages,
  );
  return attachKeyMethods(k);
};
CryptoKeyImpl.deriveKey = (
  algorithm: any,
  baseKey: any,
  derivedKeyType: any,
  extractable: boolean,
  keyUsages: string[],
) => {
  const k = native.CryptoKey_deriveKey(algorithm, baseKey, derivedKeyType, extractable, keyUsages);
  return attachKeyMethods(k);
};
CryptoKeyImpl.deriveBits = native.CryptoKey_deriveBits;
CryptoKeyImpl.decapsulateBits = native.CryptoKey_decapsulateBits;
CryptoKeyImpl.decapsulateKey = (
  decapsulationAlgorithm: any,
  decapsulationKey: any,
  ciphertext: any,
  sharedKeyAlgorithm: any,
  extractable: boolean,
  keyUsages: string[],
) => {
  const k = native.CryptoKey_decapsulateKey(
    decapsulationAlgorithm,
    decapsulationKey,
    ciphertext,
    sharedKeyAlgorithm,
    extractable,
    keyUsages,
  );
  return attachKeyMethods(k);
};
CryptoKeyImpl.Format = FormatEnum;

export { CryptoKeyImpl as CryptoKey };
