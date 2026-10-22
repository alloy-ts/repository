use napi::Result;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

/// Representation of algorithm specification for WebCrypto CryptoKey.
#[napi(object)]
pub struct CryptoKeyAlgorithm {
    pub name: String,
    pub length: Option<u32>,
    pub named_curve: Option<String>,
    pub hash: Option<String>,
    pub modulus_length: Option<u32>,
    pub public_exponent: Option<Buffer>,
}

/// NAPI binding representing a WebCrypto `CryptoKey`.
#[derive(Clone)]
#[napi]
pub struct CryptoKey {
    pub type_: String,
    pub extractable: bool,
    pub algorithm_name: String,
    pub usages: Vec<String>,
    pub handle: Vec<u8>,
}

#[napi]
impl CryptoKey {
    #[napi(constructor)]
    pub fn new(
        algorithm: CryptoKeyAlgorithm,
        extractable: bool,
        usages: Vec<String>,
    ) -> Result<Self> {
        let key_type = match algorithm.name.as_str() {
            "ECDSA" | "Ed25519" | "RSASSA-PKCS1-v1_5" | "X25519" => "private",
            _ => "secret",
        }
        .to_string();

        Ok(CryptoKey {
            type_: key_type,
            extractable,
            algorithm_name: algorithm.name,
            usages,
            handle: vec![],
        })
    }

    /// Check if an algorithm and operation are supported.
    #[napi]
    pub fn supports(
        operation: String,
        algorithm: String,
        _length_or_additional_algorithm: Option<String>,
    ) -> bool {
        match (operation.as_str(), algorithm.as_str()) {
            ("encrypt" | "decrypt", "AES-CBC" | "AES-CTR" | "AES-GCM" | "AES-OCB" | "ChaCha20-Poly1305" | "RSA-OAEP") => true,
            ("sign" | "verify", "ECDSA" | "Ed25519" | "HMAC" | "RSA-PSS" | "RSASSA-PKCS1-v1_5") => true,
            ("importKey" | "exportKey", "AES-CBC" | "AES-GCM" | "HMAC" | "ECDSA" | "Ed25519" | "X25519") => true,
            ("decapsulateBits" | "decapsulateKey", "ML-KEM-512" | "ML-KEM-768" | "ML-KEM-1024" | "MLKEM768-P256" | "MLKEM768-X25519" | "MLKEM1024-P384") => true,
            _ => false,
        }
    }

    /// Encrypt data using a CryptoKey.
    #[napi]
    pub fn encrypt(&self, _algorithm: CryptoKeyAlgorithm, _data: Buffer) -> Result<Buffer> {
        Err(napi::Error::from_reason("Encryption backend not implemented"))
    }

    /// Decrypt data using a CryptoKey.
    #[napi]
    pub fn decrypt(&self, _algorithm: CryptoKeyAlgorithm, _data: Buffer) -> Result<Buffer> {
        Err(napi::Error::from_reason("Decryption backend not implemented"))
    }

    /// Sign data using a CryptoKey.
    #[napi]
    pub fn sign(&self, _algorithm: CryptoKeyAlgorithm, _data: Buffer) -> Result<Buffer> {
        Err(napi::Error::from_reason("Sign operation not implemented"))
    }

    /// Verify signature on data using a CryptoKey.
    #[napi]
    pub fn verify(
        &self,
        _algorithm: CryptoKeyAlgorithm,
        _signature: Buffer,
        _data: Buffer,
    ) -> Result<bool> {
        Err(napi::Error::from_reason("Verify operation not implemented"))
    }

    /// Export key in specified format (e.g. jwk, raw, spki, pkcs8).
    #[napi]
    pub fn export_key(_format: String, _key: &CryptoKey) -> Result<String> {
        Err(napi::Error::from_reason("Export key not implemented"))
    }

    /// Import key from data.
    #[napi]
    pub fn import(
        _format: String,
        _key_data: String,
        _algorithm: CryptoKeyAlgorithm,
        _extractable: bool,
        _usages: Vec<String>,
    ) -> Result<CryptoKey> {
        Err(napi::Error::from_reason("Import key not implemented"))
    }

    /// Decapsulate bits from encapsulated ciphertext.
    #[napi]
    pub fn decapsulate_bits(
        &self,
        _decapsulation_algorithm: String,
        _ciphertext: Buffer,
    ) -> Result<Buffer> {
        Err(napi::Error::from_reason("Decapsulate bits not implemented"))
    }

    /// Decapsulate key from encapsulated ciphertext.
    #[napi]
    pub fn decapsulate_key(
        &self,
        _decapsulation_algorithm: String,
        _ciphertext: Buffer,
        _shared_key_algorithm: CryptoKeyAlgorithm,
        _extractable: bool,
        _usages: Vec<String>,
    ) -> Result<CryptoKey> {
        Err(napi::Error::from_reason("Decapsulate key not implemented"))
    }
}
