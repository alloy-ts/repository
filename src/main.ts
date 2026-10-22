import * as native from "../dist/index.js";

export * from "../dist/index.js";

export const Format = {
  RAW: "raw",
  RAW_SECRET: "raw-secret",
  RAW_PUBLIC: "raw-public",
  RAW_SEED: "raw-seed",
  SPKI: "spki",
  PKCS8: "pkcs8",
  JWK: "jwk",
} as const;

const NativeKey = (native as any).CryptoKey;

if (NativeKey && NativeKey.prototype) {
  const proto = NativeKey.prototype;
  Object.defineProperty(proto, "publicKey", {
    get() {
      const pk = this.getPublicKeyProp;
      return pk ?? undefined;
    },
    enumerable: true,
    configurable: true,
  });

  Object.defineProperty(proto, "privateKey", {
    get() {
      const pk = this.getPrivateKeyProp;
      return pk ?? undefined;
    },
    enumerable: true,
    configurable: true,
  });

  proto.encrypt = function (algorithm: any, key: any, data?: any) {
    if (data === undefined) {
      return NativeKey.encrypt(algorithm, this, key);
    }
    return NativeKey.encrypt(algorithm, key, data);
  };

  proto.decrypt = function (algorithm: any, key: any, data?: any) {
    if (data === undefined) {
      return NativeKey.decrypt(algorithm, this, key);
    }
    return NativeKey.decrypt(algorithm, key, data);
  };

  proto.sign = function (algorithm: any, key: any, data?: any) {
    if (data === undefined) {
      return NativeKey.sign(algorithm, this, key);
    }
    return NativeKey.sign(algorithm, key, data);
  };

  proto.verify = function (algorithm: any, key: any, signature: any, data?: any) {
    if (data === undefined) {
      return NativeKey.verify(algorithm, this, key, signature);
    }
    return NativeKey.verify(algorithm, key, signature, data);
  };

  proto.digest = function (algorithm: any, data: any) {
    return NativeKey.digest(algorithm, data);
  };

  proto.deriveBits = function (algorithm: any, baseKey?: any, length?: any) {
    if (typeof baseKey === "number" || baseKey === undefined || baseKey === null) {
      return NativeKey.deriveBits(algorithm, this, baseKey);
    }
    return NativeKey.deriveBits(algorithm, baseKey, length);
  };

  proto.deriveKey = function (
    algorithm: any,
    baseKey: any,
    derivedKeyType?: any,
    extractable?: any,
    keyUsages?: any,
  ) {
    if (derivedKeyType === undefined) {
      return NativeKey.deriveKey(algorithm, this, baseKey, extractable, keyUsages);
    }
    return NativeKey.deriveKey(algorithm, baseKey, derivedKeyType, extractable, keyUsages);
  };

  proto.wrapKey = function (format: string, key: any, wrappingKey?: any, algorithm?: any) {
    if (algorithm === undefined) {
      return NativeKey.wrapKey(format, key, this, wrappingKey);
    }
    return NativeKey.wrapKey(format, key, wrappingKey, algorithm);
  };

  proto.unwrapKey = function (
    format: string,
    wrappedKey: any,
    unwrappingKey: any,
    wrapAlgorithm?: any,
    derivedKeyAlgorithm?: any,
    extractable?: any,
    keyUsages?: any,
  ) {
    if (keyUsages === undefined) {
      return NativeKey.unwrapKey(
        format,
        wrappedKey,
        this,
        unwrappingKey,
        wrapAlgorithm,
        derivedKeyAlgorithm,
        extractable,
      );
    }
    return NativeKey.unwrapKey(
      format,
      wrappedKey,
      unwrappingKey,
      wrapAlgorithm,
      derivedKeyAlgorithm,
      extractable,
      keyUsages,
    );
  };

  proto.encapsulateBits = function (algorithm: any, publicKey?: any) {
    return NativeKey.encapsulateBits(algorithm, publicKey ?? this);
  };

  proto.decapsulateBits = function (algorithm: any, privateKey?: any, ciphertext?: any) {
    if (ciphertext === undefined) {
      return NativeKey.decapsulateBits(algorithm, this, privateKey);
    }
    return NativeKey.decapsulateBits(algorithm, privateKey, ciphertext);
  };

  proto.encapsulateKey = function (
    algorithm: any,
    publicKey?: any,
    derivedKeyType?: any,
    extractable?: any,
    keyUsages?: any,
  ) {
    return NativeKey.encapsulateKey(algorithm, publicKey ?? this, derivedKeyType, extractable, keyUsages);
  };

  proto.decapsulateKey = function (
    algorithm: any,
    privateKey?: any,
    ciphertext?: any,
    derivedKeyType?: any,
    extractable?: any,
    keyUsages?: any,
  ) {
    return NativeKey.decapsulateKey(
      algorithm,
      privateKey ?? this,
      ciphertext,
      derivedKeyType,
      extractable,
      keyUsages,
    );
  };
}

const origImportKey = NativeKey.importKey;

function WrappedCryptoKey(this: any, ...args: any[]) {
  return Reflect.construct(NativeKey, args, new.target || WrappedCryptoKey);
}

Object.setPrototypeOf(WrappedCryptoKey, NativeKey);
WrappedCryptoKey.prototype = NativeKey.prototype;

Object.defineProperty(WrappedCryptoKey, "Format", {
  value: Format,
  writable: true,
  configurable: true,
  enumerable: true,
});

Object.defineProperty(WrappedCryptoKey, "importKey", {
  value: function (format: string, keyData: any, algorithm: any, extractable: boolean, usages: string[]) {
    let data = keyData;
    if (
      format === "jwk" &&
      typeof keyData === "object" &&
      keyData !== null &&
      !(keyData instanceof Uint8Array) &&
      !Buffer.isBuffer(keyData)
    ) {
      data = JSON.stringify(keyData);
    }
    return origImportKey.call(NativeKey, format, data, algorithm, extractable, usages);
  },
  writable: true,
  configurable: true,
});

export const CryptoKey = WrappedCryptoKey as any;

export class SignatureWrapper {
  private static get Native() {
    return (native as any).Signature;
  }

  constructor(arg1: any, keyFormat?: string) {
    const fmt = typeof arg1 === "string" ? arg1 : keyFormat ?? "raw";
    const bytes = Buffer.isBuffer(arg1) || arg1 instanceof Uint8Array ? arg1 : null;
    const inst = new SignatureWrapper.Native(bytes ? Buffer.from(bytes) : fmt, fmt);
    if (bytes) {
      inst.setBytes(Buffer.from(bytes));
    }
    return inst;
  }
}

export const Signature = SignatureWrapper as any;

export function keyFormats(algorithm: string) {
  const res = (native as any).keyFormats?.(algorithm);
  return res ?? undefined;
}
