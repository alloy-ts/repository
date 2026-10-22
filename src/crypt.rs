use napi::Result;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

/// Supported key formats for import/export.
#[napi]
pub enum KeyFormat {
    Raw,
    RawSecret,
    RawPublic,
    RawSeed,
    Spki,
    Pkcs8,
    Jwk,
}

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
    #[napi(js_name = "type")]
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

    /// Public key getter for asymmetric key pair destructuring (`{ publicKey, privateKey }`).
    #[napi(getter, js_name = "publicKey")]
    pub fn public_key(&self) -> Self {
        let mut pub_key = self.clone();
        pub_key.type_ = "public".to_string();
        pub_key
    }

    /// Private key getter for asymmetric key pair destructuring (`{ publicKey, privateKey }`).
    #[napi(getter, js_name = "privateKey")]
    pub fn private_key(&self) -> Self {
        let mut priv_key = self.clone();
        priv_key.type_ = "private".to_string();
        priv_key
    }

    /// Key format property accessible via `CryptoKey.Format`.
    #[napi(getter, js_name = "Format")]
    pub fn format_enum() -> KeyFormat {
        KeyFormat::Raw
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
            ("importKey" | "exportKey" | "generateKey", "AES-CBC" | "AES-GCM" | "HMAC" | "ECDSA" | "Ed25519" | "X25519") => true,
            ("decapsulateBits" | "decapsulateKey" | "encapsulateBits" | "encapsulateKey", "ML-KEM-512" | "ML-KEM-768" | "ML-KEM-1024" | "MLKEM768-P256" | "MLKEM768-X25519" | "MLKEM1024-P384") => true,
            _ => false,
        }
    }

    /// Static method: export key in specified format.
    #[napi]
    pub fn export_key(_format: String, _key: &CryptoKey) -> Result<String> {
        Err(napi::Error::from_reason("Export key backend not implemented"))
    }

    /// Static method: import key from data.
    #[napi]
    pub fn import_key(
        _format: String,
        _key_data: String,
        _algorithm: CryptoKeyAlgorithm,
        _extractable: bool,
        _usages: Vec<String>,
    ) -> Result<CryptoKey> {
        Err(napi::Error::from_reason("Import key backend not implemented"))
    }

    /// Static method: derive a key from a base key.
    #[napi]
    pub fn derive_key(
        _algorithm: CryptoKeyAlgorithm,
        _base_key: &CryptoKey,
        derived_key_type: CryptoKeyAlgorithm,
        extractable: bool,
        key_usages: Vec<String>,
    ) -> Result<CryptoKey> {
        CryptoKey::new(derived_key_type, extractable, key_usages)
    }

    /// Static method: derive bits from a base key.
    #[napi]
    pub fn derive_bits(
        _algorithm: CryptoKeyAlgorithm,
        _base_key: &CryptoKey,
        _length: Option<u32>,
    ) -> Result<Buffer> {
        Err(napi::Error::from_reason("Derive bits backend not implemented"))
    }

    /// Static method: wrap a key with a wrapping key.
    #[napi]
    pub fn wrap_key(
        _format: String,
        _key: &CryptoKey,
        _wrapping_key: &CryptoKey,
        _wrap_algorithm: String,
    ) -> Result<Buffer> {
        Err(napi::Error::from_reason("Wrap key backend not implemented"))
    }

    /// Static method: unwrap a wrapped key.
    #[napi]
    pub fn unwrap_key(
        _format: String,
        _wrapped_key: Buffer,
        _wrapping_key: &CryptoKey,
        _unwrap_algorithm: String,
        unwrapped_key_algorithm: CryptoKeyAlgorithm,
        extractable: bool,
        key_usages: Vec<String>,
    ) -> Result<CryptoKey> {
        CryptoKey::new(unwrapped_key_algorithm, extractable, key_usages)
    }

    /// Static method: compute digest of data.
    #[napi]
    pub fn digest(_algorithm: String, _data: Buffer) -> Result<Buffer> {
        Err(napi::Error::from_reason("Digest backend not implemented"))
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

/// A cryptographic signature representation created with a key format.
#[napi]
pub struct Signature {
    pub key_format: KeyFormat,
}

#[napi]
impl Signature {
    #[napi(constructor)]
    pub fn new(key_format: KeyFormat) -> Self {
        Signature { key_format }
    }
}
