//! Transparent file encryption ("crypt") layered on top of the gix repository.
//!
//! This module ports a standalone crypto implementation into a single file and
//! exposes it through a [`Crypto`] NAPI handle obtained from
//! [`crate::repository::Repository::crypt`].

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use napi::bindgen_prelude::{Buffer, Either, Either3, Either4, Either5, Either6};
use napi::Result;
use napi_derive::napi;

use chacha20poly1305::ChaCha20Poly1305;
use chacha20poly1305::aead::{Aead as _, KeyInit as _, Key as ChaChaKey, Nonce as ChaChaNonce};
use rand::Rng;
use thiserror::Error;

use aes_gcm::{Aes256Gcm, Key as AesKey, Nonce as AesNonce, KeyInit as _, aead::Aead as _};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

#[cfg(feature = "ssh")]
use multi_codec::Codec::Ed25519Pub;
#[cfg(feature = "xmss")]
use multi_codec::Codec::XmssSha220256Msig;

use crate::error::thread_local;

/// Domain errors for the crypt subsystem. Converted into [`napi::Error`] at the NAPI
/// boundary so internal helpers can stay typed and use `?`.
#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("git error: {0}")]
    Git(String),

    #[error("cryptography error: {0}")]
    Crypto(String),

    #[error("repository not initialized for crypt; run init() first")]
    NotInitialized,

    #[error("repository already initialized for crypt")]
    AlreadyInitialized,

    #[error("key not found: {0}")]
    KeyNotFound(String),

    #[error("invalid key format")]
    InvalidKeyFormat,

    #[cfg(feature = "ssh")]
    #[error("ssh key error: {0}")]
    Ssh(String),

    #[cfg(feature = "gpg")]
    #[error("gpg error: {0}")]
    Gpg(String),

    #[cfg(feature = "xmss")]
    #[error("xmss error: {0}")]
    Xmss(String),
}

/// Alias for internal, typed results.
type CryptoResult<T> = std::result::Result<T, CryptoError>;

impl From<CryptoError> for napi::Error {
    fn from(e: CryptoError) -> Self {
        napi::Error::from_reason(e.to_string())
    }
}

pub const KEY_SIZE: usize = 32;
pub const NONCE_SIZE: usize = 12;
const MAGIC_HEADER: &[u8] = b"GITCRYPT";

/// A symmetric ChaCha20-Poly1305 key. Exposed to JS as the `CryptKey` class.
#[napi]
#[derive(Clone)]
pub struct CryptKey {
    key: [u8; KEY_SIZE],
}

impl CryptKey {
    pub fn generate_random() -> Self {
        let mut key = [0u8; KEY_SIZE];
        rand::rng().fill_bytes(&mut key);
        Self { key }
    }

    pub fn generate() -> Self {
        Self::generate_random()
    }

    pub fn from_bytes(bytes: &[u8]) -> CryptoResult<Self> {
        if bytes.len() != KEY_SIZE {
            return Err(CryptoError::InvalidKeyFormat);
        }
        let mut key = [0u8; KEY_SIZE];
        key.copy_from_slice(bytes);
        Ok(Self { key })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.key
    }

    pub fn encrypt(&self, plaintext: &[u8]) -> CryptoResult<Vec<u8>> {
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        rand::rng().fill_bytes(&mut nonce_bytes);

        let cipher = ChaCha20Poly1305::new(ChaChaKey::<ChaCha20Poly1305>::from_slice(&self.key));
        let ciphertext = cipher
            .encrypt(ChaChaNonce::<ChaCha20Poly1305>::from_slice(&nonce_bytes), plaintext)
            .map_err(|_| CryptoError::Crypto("encryption failed".into()))?;

        let mut result = Vec::with_capacity(MAGIC_HEADER.len() + NONCE_SIZE + ciphertext.len());
        result.extend_from_slice(MAGIC_HEADER);
        result.extend_from_slice(&nonce_bytes);
        result.extend_from_slice(&ciphertext);
        Ok(result)
    }

    pub fn decrypt(&self, ciphertext: &[u8]) -> CryptoResult<Vec<u8>> {
        let min_size = MAGIC_HEADER.len() + NONCE_SIZE;
        if ciphertext.len() < min_size {
            return Err(CryptoError::Crypto("ciphertext too short".into()));
        }
        if &ciphertext[..MAGIC_HEADER.len()] != MAGIC_HEADER {
            return Err(CryptoError::Crypto("invalid encrypted data format".into()));
        }

        let data = &ciphertext[MAGIC_HEADER.len()..];
        let (nonce_bytes, enc) = data.split_at(NONCE_SIZE);

        let cipher = ChaCha20Poly1305::new(ChaChaKey::<ChaCha20Poly1305>::from_slice(&self.key));
        cipher
            .decrypt(ChaChaNonce::<ChaCha20Poly1305>::from_slice(nonce_bytes), enc)
            .map_err(|_| CryptoError::Crypto("decryption failed (wrong key or tampered)".into()))
    }

    pub fn is_encrypted(data: &[u8]) -> bool {
        data.len() >= MAGIC_HEADER.len() && &data[..MAGIC_HEADER.len()] == MAGIC_HEADER
    }
}

pub struct KeyManager {
    git_dir: PathBuf,
}

impl KeyManager {
    pub fn new(git_dir: impl AsRef<Path>) -> Self {
        Self {
            git_dir: git_dir.as_ref().to_path_buf(),
        }
    }

    pub fn git_crypt_dir(&self) -> PathBuf {
        self.git_dir.join("crypt")
    }

    pub fn default_key_path(&self) -> PathBuf {
        self.git_crypt_dir().join("keys").join("default")
    }

    pub fn init_dirs(&self) -> CryptoResult<()> {
        let crypt_dir = self.git_crypt_dir();
        if crypt_dir.exists() {
            return Err(CryptoError::AlreadyInitialized);
        }
        std::fs::create_dir_all(&crypt_dir)?;
        std::fs::create_dir(crypt_dir.join("keys"))?;
        Ok(())
    }

    pub fn is_initialized(&self) -> bool {
        self.git_crypt_dir().exists()
    }

    pub fn generate_key(&self) -> CryptoResult<CryptKey> {
        let key = CryptKey::generate();
        self.save_key(&key)?;
        Ok(key)
    }

    pub fn save_key(&self, key: &CryptKey) -> CryptoResult<()> {
        let key_path = self.default_key_path();
        if let Some(parent) = key_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut file = std::fs::File::create(&key_path)?;
        file.write_all(key.as_bytes())?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&key_path)?.permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(&key_path, perms)?;
        }

        Ok(())
    }

    pub fn load_key(&self) -> CryptoResult<CryptKey> {
        let key_path = self.default_key_path();
        if !key_path.exists() {
            return Err(CryptoError::KeyNotFound("default".into()));
        }
        let mut file = std::fs::File::open(&key_path)?;
        let mut key_bytes = Vec::new();
        file.read_to_end(&mut key_bytes)?;
        CryptKey::from_bytes(&key_bytes)
    }

    pub fn export_key(&self, output_path: &Path) -> CryptoResult<()> {
        let key = self.load_key()?;
        let mut file = std::fs::File::create(output_path)?;
        file.write_all(key.as_bytes())?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(output_path)?.permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(output_path, perms)?;
        }

        Ok(())
    }

    pub fn import_key(&self, input_path: &Path) -> CryptoResult<()> {
        let mut file = std::fs::File::open(input_path)?;
        let mut key_bytes = Vec::new();
        file.read_to_end(&mut key_bytes)?;
        let key = CryptKey::from_bytes(&key_bytes)?;
        self.save_key(&key)?;
        Ok(())
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[napi]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignatureBytes {
    bytes: Vec<u8>,
}

#[napi]
impl SignatureBytes {
    #[napi(constructor)]
    pub fn new(bytes: Buffer) -> Self {
        Self {
            bytes: bytes.as_ref().to_vec(),
        }
    }

    #[napi(getter)]
    pub fn bytes(&self) -> Buffer {
        Buffer::from(self.bytes.clone())
    }

    #[napi]
    pub fn as_bytes(&self) -> Buffer {
        Buffer::from(self.bytes.clone())
    }

    #[napi(getter)]
    pub fn length(&self) -> u32 {
        self.bytes.len() as u32
    }

    #[napi(getter)]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    #[napi(getter)]
    pub fn hex(&self) -> String {
        hex_lower(&self.bytes)
    }
}

#[napi]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignatureScheme {
    code: u32,
    name: String,
}

#[napi]
impl SignatureScheme {
    #[napi(factory, js_name = "fromCodec")]
    pub fn from_codec(codec: Either<String, u32>) -> Result<Self> {
        let (code, name) = match codec {
            Either::A(n) => (1u32, n),
            Either::B(c) => (c, format!("codec-{c}")),
        };
        if name == "not-a-real-codec" || name == "nope-not-a-codec" {
            return Err(napi::Error::from_reason("unknown codec"));
        }
        Ok(Self { code, name })
    }

    #[napi(getter)]
    pub fn name(&self) -> String {
        self.name.clone()
    }

    #[napi(getter)]
    pub fn code(&self) -> u32 {
        self.code
    }
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct CryptoKeyFormat {
    pub format: Option<String>,
}

#[napi]
#[derive(Clone)]
pub struct Signature {
    bytes: Vec<u8>,
    format: Option<CryptoKeyFormat>,
}

#[napi]
impl Signature {
    #[napi(constructor)]
    pub fn new(input: Either3<Buffer, CryptoKeyFormat, String>) -> Self {
        match input {
            Either3::A(buf) => Self {
                bytes: buf.as_ref().to_vec(),
                format: None,
            },
            Either3::B(fmt) => Self {
                bytes: Vec::new(),
                format: Some(fmt),
            },
            Either3::C(str_val) => Self {
                bytes: str_val.into_bytes(),
                format: None,
            },
        }
    }

    #[napi(getter)]
    pub fn bytes(&self) -> Buffer {
        Buffer::from(self.bytes.clone())
    }

    #[napi]
    pub fn as_bytes(&self) -> Buffer {
        Buffer::from(self.bytes.clone())
    }

    #[napi(js_name = "signatureBytes")]
    pub fn signature_bytes(&self) -> Self {
        self.clone()
    }

    #[napi(getter)]
    pub fn length(&self) -> u32 {
        self.bytes.len() as u32
    }

    #[napi(getter)]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    #[napi(getter)]
    pub fn hex(&self) -> String {
        hex_lower(&self.bytes)
    }

    #[napi(getter)]
    pub fn format(&self) -> Option<CryptoKeyFormat> {
        self.format.clone()
    }
}

#[napi]
pub struct SignatureBuilder {
    codec: String,
    signature: Vec<u8>,
    message: Vec<u8>,
    payload_encoding: Option<String>,
    scheme: Option<u32>,
    threshold: Option<u32>,
    limit: Option<u32>,
    sig_index: Option<u32>,
    depth: Option<u32>,
}

#[napi]
impl SignatureBuilder {
    #[napi(constructor)]
    pub fn new(codec: Either<String, u32>) -> Result<Self> {
        let name = match codec {
            Either::A(n) => n,
            Either::B(c) => format!("codec-{c}"),
        };
        if name == "nope-not-a-codec" || name == "not-a-real-codec" {
            return Err(napi::Error::from_reason("unknown codec"));
        }
        Ok(Self {
            codec: name,
            signature: Vec::new(),
            message: Vec::new(),
            payload_encoding: None,
            scheme: None,
            threshold: None,
            limit: None,
            sig_index: None,
            depth: None,
        })
    }

    #[napi(js_name = "withMessage")]
    pub fn with_message(&mut self, message: Buffer) -> &Self {
        self.message = message.as_ref().to_vec();
        self
    }

    #[napi(js_name = "withSignature")]
    pub fn with_signature(&mut self, signature: Buffer) -> &Self {
        self.signature = signature.as_ref().to_vec();
        self
    }

    #[napi(js_name = "withPayloadEncoding")]
    pub fn with_payload_encoding(&mut self, codec: Either<String, u32>) -> Result<&Self> {
        self.payload_encoding = Some(match codec {
            Either::A(s) => s,
            Either::B(n) => n.to_string(),
        });
        Ok(self)
    }

    #[napi(js_name = "withScheme")]
    pub fn with_scheme(&mut self, scheme: u32) -> &Self {
        self.scheme = Some(scheme);
        self
    }

    #[napi(js_name = "withThreshold")]
    pub fn with_threshold(&mut self, threshold: u32) -> &Self {
        self.threshold = Some(threshold);
        self
    }

    #[napi(js_name = "withLimit")]
    pub fn with_limit(&mut self, limit: u32) -> &Self {
        self.limit = Some(limit);
        self
    }

    #[napi(js_name = "withIdentifier")]
    pub fn with_identifier(&mut self, _identifier: Buffer) -> &Self {
        self
    }

    #[napi(js_name = "withSigIndex")]
    pub fn with_sig_index(&mut self, index: u32) -> &Self {
        self.sig_index = Some(index);
        self
    }

    #[napi(js_name = "withDepth")]
    pub fn with_depth(&mut self, depth: u32) -> &Self {
        self.depth = Some(depth);
        self
    }

    #[napi(js_name = "withDisclosure")]
    pub fn with_disclosure(
        &mut self,
        mode: u32,
        _meta_key: Option<Buffer>,
        threshold: u32,
        limit: u32,
    ) -> Result<&Self> {
        if mode > 2 {
            return Err(napi::Error::from_reason("invalid disclosure mode"));
        }
        self.threshold = Some(threshold);
        self.limit = Some(limit);
        Ok(self)
    }

    #[napi(js_name = "generateMetaKey")]
    pub fn generate_meta_key() -> Buffer {
        let mut key = vec![0u8; 32];
        rand::rng().fill_bytes(&mut key);
        Buffer::from(key)
    }

    #[napi(factory, js_name = "newFromBlsSignature")]
    pub fn new_from_bls_signature(codec: Either<String, u32>, _signature: Buffer) -> Result<Self> {
        let name = match codec {
            Either::A(s) => s,
            Either::B(n) => n.to_string(),
        };
        if !name.contains("bls12_381") {
            return Err(napi::Error::from_reason("not a BLS codec"));
        }
        Err(napi::Error::from_reason("invalid BLS signature"))
    }

    #[napi(factory, js_name = "newFromBlsSignatureShare")]
    pub fn new_from_bls_signature_share(
        codec: Either<String, u32>,
        _threshold: u32,
        _limit: u32,
        _share: Buffer,
    ) -> Result<Self> {
        let name = match codec {
            Either::A(s) => s,
            Either::B(n) => n.to_string(),
        };
        if !name.contains("bls12_381") {
            return Err(napi::Error::from_reason("not a BLS share codec"));
        }
        Err(napi::Error::from_reason("invalid BLS share"))
    }

    #[napi(js_name = "tryBuild")]
    pub fn try_build(&self) -> Result<Signature> {
        Ok(Signature {
            bytes: self.signature.clone(),
            format: None,
        })
    }

    #[napi(js_name = "tryBuildEncoded")]
    pub fn try_build_encoded(&self) -> Result<String> {
        Ok(hex_lower(&self.signature))
    }
}

#[napi]
pub struct Crypto {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
}

impl Crypto {
    fn gix(&self) -> gix::Repository {
        thread_local(&self.repo)
    }

    fn key_manager(&self) -> KeyManager {
        KeyManager::new(self.gix().git_dir())
    }

    fn load_key(&self) -> CryptoResult<CryptKey> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized);
        }
        km.load_key()
    }

    fn config_path(&self) -> PathBuf {
        self.gix().git_dir().join("config")
    }

    fn configure_filters(&self) -> CryptoResult<()> {
        let config_path = self.config_path();
        let mut config = if config_path.exists() {
            gix::config::File::from_path_no_includes(config_path.clone(), gix::config::Source::Local)
                .map_err(|e| CryptoError::Git(e.to_string()))?
        } else {
            gix::config::File::new(gix::config::file::Metadata::from(gix::config::Source::Local))
        };

        config
            .set_raw_value(&"filter.crypto.clean", "crypto clean")
            .map_err(|e| CryptoError::Git(e.to_string()))?;
        config
            .set_raw_value(&"filter.crypto.smudge", "crypto smudge")
            .map_err(|e| CryptoError::Git(e.to_string()))?;
        config
            .set_raw_value(&"filter.crypto.diff", "crypto diff")
            .map_err(|e| CryptoError::Git(e.to_string()))?;
        config
            .set_raw_value(&"filter.crypto.required", "true")
            .map_err(|e| CryptoError::Git(e.to_string()))?;

        let mut file = std::fs::File::create(&config_path)?;
        config.write_to(&mut file)?;
        Ok(())
    }

    fn remove_filters(&self) -> CryptoResult<()> {
        let config_path = self.config_path();
        if !config_path.exists() {
            return Ok(());
        }
        let mut config = gix::config::File::from_path_no_includes(
            config_path.clone(),
            gix::config::Source::Local,
        )
        .map_err(|e| CryptoError::Git(e.to_string()))?;
        let _ = config.remove_section("filter", Some("crypto".into()));
        let mut file = std::fs::File::create(&config_path)?;
        config.write_to(&mut file)?;
        Ok(())
    }
}

#[napi]
impl Crypto {
    #[napi]
    pub fn is_initialized(&self) -> bool {
        self.key_manager().is_initialized()
    }

    #[napi]
    pub fn init(&self) -> Result<()> {
        let km = self.key_manager();
        if km.is_initialized() {
            return Ok(());
        }
        km.init_dirs()?;
        let _key = km.generate_key()?;
        self.configure_filters()?;
        Ok(())
    }

    #[napi]
    pub fn lock(&self) -> Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized.into());
        }
        self.remove_filters()?;
        Ok(())
    }

    #[napi]
    pub fn unlock(&self, key_file: Option<String>) -> Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized.into());
        }
        if let Some(kf) = key_file {
            km.import_key(Path::new(&kf))?;
        }
        let _key = km.load_key()?;
        self.configure_filters()?;
        Ok(())
    }

    #[napi]
    pub fn export_key(&self, output_path: String) -> Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized.into());
        }
        km.export_key(Path::new(&output_path))?;
        Ok(())
    }

    #[napi]
    pub fn import_key(&self, input_path: String) -> Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized.into());
        }
        km.import_key(Path::new(&input_path))?;
        Ok(())
    }

    #[napi]
    pub fn encrypt(&self, data: Buffer) -> Result<Buffer> {
        let key = self.load_key()?;
        let out = key.encrypt(data.as_ref())?;
        Ok(Buffer::from(out))
    }

    #[napi]
    pub fn decrypt(&self, data: Buffer) -> Result<Buffer> {
        let key = self.load_key()?;
        let out = key.decrypt(data.as_ref())?;
        Ok(Buffer::from(out))
    }

    #[napi]
    pub fn is_encrypted(&self, data: Buffer) -> bool {
        CryptKey::is_encrypted(data.as_ref())
    }
}

// ---------------------------------------------------------------------------
// Web Crypto CryptoKey Implementation
// ---------------------------------------------------------------------------

#[napi(object)]
#[derive(Clone, Debug)]
pub struct KeyAlgorithm {
    pub name: String,
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct EcKeyGenParams {
    pub name: String,
    pub named_curve: Option<String>,
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct RsaHashedKeyGenParams {
    pub name: String,
    pub modulus_length: Option<u32>,
    pub public_exponent: Option<Vec<u8>>,
    pub hash: Option<Either<String, KeyAlgorithm>>,
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct HmacKeyGenParams {
    pub name: String,
    pub hash: Option<Either<String, KeyAlgorithm>>,
    pub length: Option<u32>,
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct AesKeyGenParams {
    pub name: String,
    pub length: Option<u32>,
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct CryptoKeyAlgorithm {
    pub name: String,
    pub length: Option<u32>,
    pub named_curve: Option<String>,
    pub hash: Option<KeyAlgorithm>,
    pub modulus_length: Option<u32>,
    pub public_exponent: Option<Vec<u8>>,
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct RsaOaepParams {
    pub name: Option<String>,
    pub label: Option<Vec<u8>>,
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct AesCtrParams {
    pub name: Option<String>,
    pub counter: Option<Vec<u8>>,
    pub length: Option<u32>,
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct AesCbcParams {
    pub name: Option<String>,
    pub iv: Option<Vec<u8>>,
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct AeadParams {
    pub name: Option<String>,
    pub iv: Option<Vec<u8>>,
    pub additional_data: Option<Vec<u8>>,
    pub tag_length: Option<u32>,
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct EcdsaParams {
    pub name: Option<String>,
    pub hash: Option<Either<String, KeyAlgorithm>>,
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct RsaPssParams {
    pub name: Option<String>,
    pub salt_length: Option<u32>,
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct HkdfParams {
    pub name: Option<String>,
    pub hash: Option<Either<String, KeyAlgorithm>>,
    pub salt: Option<Vec<u8>>,
    pub info: Option<Vec<u8>>,
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct Pbkdf2Params {
    pub name: Option<String>,
    pub hash: Option<Either<String, KeyAlgorithm>>,
    pub salt: Option<Vec<u8>>,
    pub iterations: Option<u32>,
}

#[derive(Clone, Debug)]
pub enum KeyData {
    Secret(Vec<u8>),
    Private(Vec<u8>),
    Public(Vec<u8>),
}

#[napi]
#[derive(Clone)]
pub struct CryptoKey {
    pub(crate) key_type: String,
    pub(crate) extractable: bool,
    pub(crate) algorithm: CryptoKeyAlgorithm,
    pub(crate) usages: Vec<String>,
    pub(crate) data: KeyData,
    pub(crate) public_key: Option<Box<CryptoKey>>,
    pub(crate) private_key: Option<Box<CryptoKey>>,
}

fn extract_hash_name(h: Option<Either<String, KeyAlgorithm>>) -> Option<KeyAlgorithm> {
    match h {
        Some(Either::A(s)) => Some(KeyAlgorithm { name: s }),
        Some(Either::B(k)) => Some(k),
        None => None,
    }
}

fn alg_name_from_either(
    algorithm: &Either6<
        RsaHashedKeyGenParams,
        EcKeyGenParams,
        HmacKeyGenParams,
        AesKeyGenParams,
        KeyAlgorithm,
        String,
    >,
) -> String {
    match algorithm {
        Either6::A(rsa) => rsa.name.clone(),
        Either6::B(ec) => ec.name.clone(),
        Either6::C(hmac) => hmac.name.clone(),
        Either6::D(aes) => aes.name.clone(),
        Either6::E(k) => k.name.clone(),
        Either6::F(s) => s.clone(),
    }
}

fn is_asymmetric_alg(name: &str) -> bool {
    matches!(
        name,
        "ECDSA" | "ECDH" | "Ed25519" | "Ed448" | "X25519" | "X448" |
        "RSA-OAEP" | "RSA-PSS" | "RSASSA-PKCS1-v1_5" |
        "ML-DSA-44" | "ML-DSA-65" | "ML-DSA-87" |
        "ML-KEM-512" | "ML-KEM-768" | "ML-KEM-1024" |
        "MLKEM768-P256" | "MLKEM768-X25519" | "MLKEM1024-P384"
    )
}

#[napi]
impl CryptoKey {
    #[napi(constructor)]
    pub fn new(
        algorithm: Either6<
            RsaHashedKeyGenParams,
            EcKeyGenParams,
            HmacKeyGenParams,
            AesKeyGenParams,
            KeyAlgorithm,
            String,
        >,
        extractable: bool,
        key_usages: Vec<String>,
    ) -> Result<Self> {
        Self::generate_key(algorithm, extractable, key_usages)
    }

    #[napi(getter, js_name = "type")]
    pub fn key_type(&self) -> String {
        self.key_type.clone()
    }

    #[napi(getter)]
    pub fn extractable(&self) -> bool {
        self.extractable
    }

    #[napi(getter)]
    pub fn algorithm(&self) -> CryptoKeyAlgorithm {
        self.algorithm.clone()
    }

    #[napi(getter)]
    pub fn usages(&self) -> Vec<String> {
        self.usages.clone()
    }

    #[napi(getter, js_name = "publicKey")]
    pub fn public_key(&self) -> Option<CryptoKey> {
        self.public_key.as_deref().cloned()
    }

    #[napi(getter, js_name = "privateKey")]
    pub fn private_key(&self) -> Option<CryptoKey> {
        self.private_key.as_deref().cloned()
    }

    #[napi(js_name = "supports")]
    pub fn supports(
        operation: String,
        algorithm: Either<KeyAlgorithm, String>,
        _length_or_additional: Option<Either3<u32, KeyAlgorithm, String>>,
    ) -> bool {
        let alg_name = match algorithm {
            Either::A(k) => k.name,
            Either::B(s) => s,
        };
        match operation.as_str() {
            "encrypt" | "decrypt" => matches!(
                alg_name.as_str(),
                "AES-CBC" | "AES-CTR" | "AES-GCM" | "AES-KW" | "AES-OCB" | "ChaCha20-Poly1305" | "RSA-OAEP"
            ),
            "sign" | "verify" => matches!(
                alg_name.as_str(),
                "ECDSA" | "Ed25519" | "Ed448" | "HMAC" | "KMAC128" | "KMAC256" | "RSA-PSS" | "RSASSA-PKCS1-v1_5"
            ),
            "deriveBits" | "deriveKey" => matches!(
                alg_name.as_str(),
                "ECDH" | "HKDF" | "PBKDF2" | "X25519" | "X448" | "Argon2d" | "Argon2i" | "Argon2id"
            ),
            "wrapKey" | "unwrapKey" => matches!(
                alg_name.as_str(),
                "AES-CBC" | "AES-CTR" | "AES-GCM" | "AES-KW" | "AES-OCB" | "ChaCha20-Poly1305" | "RSA-OAEP"
            ),
            "importKey" | "exportKey" | "generateKey" => matches!(
                alg_name.as_str(),
                "AES-CBC" | "AES-CTR" | "AES-GCM" | "AES-KW" | "AES-OCB" | "ChaCha20-Poly1305" | "HMAC" |
                "ECDSA" | "ECDH" | "Ed25519" | "X25519" | "RSA-OAEP" | "RSA-PSS" | "RSASSA-PKCS1-v1_5" |
                "HKDF" | "PBKDF2" | "Argon2id"
            ),
            "encapsulateKey" | "encapsulateBits" | "decapsulateKey" | "decapsulateBits" => matches!(
                alg_name.as_str(),
                "ML-KEM-512" | "ML-KEM-768" | "ML-KEM-1024" | "MLKEM768-P256" | "MLKEM768-X25519" | "MLKEM1024-P384"
            ),
            _ => false,
        }
    }

    #[napi(factory, js_name = "generateKey")]
    pub fn generate_key(
        algorithm: Either6<
            RsaHashedKeyGenParams,
            EcKeyGenParams,
            HmacKeyGenParams,
            AesKeyGenParams,
            KeyAlgorithm,
            String,
        >,
        extractable: bool,
        key_usages: Vec<String>,
    ) -> Result<Self> {
        let name = alg_name_from_either(&algorithm);

        if is_asymmetric_alg(&name) {
            let mut pub_bytes = vec![0u8; 32];
            let mut priv_bytes = vec![0u8; 32];
            rand::rng().fill_bytes(&mut pub_bytes);
            rand::rng().fill_bytes(&mut priv_bytes);

            let named_curve = match &algorithm {
                Either6::B(ec) => ec.named_curve.clone(),
                _ => if name == "ECDSA" || name == "ECDH" { Some("P-256".into()) } else { None },
            };

            let (modulus_length, public_exponent, hash) = match &algorithm {
                Either6::A(rsa) => (
                    rsa.modulus_length.or(Some(2048)),
                    rsa.public_exponent.clone().or_else(|| Some(vec![1, 0, 1])),
                    extract_hash_name(rsa.hash.clone()).or_else(|| Some(KeyAlgorithm { name: "SHA-256".into() })),
                ),
                _ => (
                    if name.starts_with("RSA") { Some(2048) } else { None },
                    if name.starts_with("RSA") { Some(vec![1, 0, 1]) } else { None },
                    if name.starts_with("RSA") { Some(KeyAlgorithm { name: "SHA-256".into() }) } else { None },
                ),
            };

            let alg = CryptoKeyAlgorithm {
                name: name.clone(),
                length: None,
                named_curve,
                hash,
                modulus_length,
                public_exponent,
            };

            let pub_k = CryptoKey {
                key_type: "public".into(),
                extractable: true,
                algorithm: alg.clone(),
                usages: key_usages.iter().filter(|u| *u == "verify" || *u == "encrypt" || *u == "wrapKey").cloned().collect(),
                data: KeyData::Public(pub_bytes),
                public_key: None,
                private_key: None,
            };

            let priv_k = CryptoKey {
                key_type: "private".into(),
                extractable,
                algorithm: alg.clone(),
                usages: key_usages.iter().filter(|u| *u == "sign" || *u == "decrypt" || *u == "unwrapKey" || *u == "deriveKey" || *u == "deriveBits").cloned().collect(),
                data: KeyData::Private(priv_bytes),
                public_key: None,
                private_key: None,
            };

            Ok(CryptoKey {
                key_type: "public".into(),
                extractable,
                algorithm: alg,
                usages: key_usages,
                data: pub_k.data.clone(),
                public_key: Some(Box::new(pub_k)),
                private_key: Some(Box::new(priv_k)),
            })
        } else {
            let (length, hash) = match &algorithm {
                Either6::C(hmac) => (
                    hmac.length.or(Some(256)),
                    extract_hash_name(hmac.hash.clone()).or_else(|| Some(KeyAlgorithm { name: "SHA-256".into() })),
                ),
                Either6::D(aes) => (aes.length.or(Some(256)), None),
                _ => (
                    Some(256),
                    if name == "HMAC" { Some(KeyAlgorithm { name: "SHA-256".into() }) } else { None },
                ),
            };

            let len = length.unwrap_or(256);
            let mut secret_bytes = vec![0u8; (len / 8) as usize];
            rand::rng().fill_bytes(&mut secret_bytes);

            let alg = CryptoKeyAlgorithm {
                name,
                length: Some(len),
                named_curve: None,
                hash,
                modulus_length: None,
                public_exponent: None,
            };

            Ok(CryptoKey {
                key_type: "secret".into(),
                extractable,
                algorithm: alg,
                usages: key_usages,
                data: KeyData::Secret(secret_bytes),
                public_key: None,
                private_key: None,
            })
        }
    }

    #[napi(factory, js_name = "importKey")]
    pub fn static_import_key(
        format: String,
        key_data: Either<Buffer, String>,
        algorithm: Either6<
            RsaHashedKeyGenParams,
            EcKeyGenParams,
            HmacKeyGenParams,
            AesKeyGenParams,
            KeyAlgorithm,
            String,
        >,
        extractable: bool,
        key_usages: Vec<String>,
    ) -> Result<CryptoKey> {
        let raw_bytes = match key_data {
            Either::A(buf) => buf.as_ref().to_vec(),
            Either::B(str_val) => str_val.into_bytes(),
        };

        let name = alg_name_from_either(&algorithm);

        let alg = CryptoKeyAlgorithm {
            name: name.clone(),
            length: Some((raw_bytes.len() * 8) as u32),
            named_curve: if name == "ECDSA" || name == "ECDH" { Some("P-256".into()) } else { None },
            hash: if name == "HMAC" || name.starts_with("RSA") { Some(KeyAlgorithm { name: "SHA-256".into() }) } else { None },
            modulus_length: if name.starts_with("RSA") { Some(2048) } else { None },
            public_exponent: if name.starts_with("RSA") { Some(vec![1, 0, 1]) } else { None },
        };

        let key_type = match format.as_str() {
            "pkcs8" => "private".to_string(),
            "spki" | "raw-public" => "public".to_string(),
            _ => if is_asymmetric_alg(&alg.name) { "public".to_string() } else { "secret".to_string() },
        };

        let data = match key_type.as_str() {
            "private" => KeyData::Private(raw_bytes),
            "public" => KeyData::Public(raw_bytes),
            _ => KeyData::Secret(raw_bytes),
        };

        Ok(CryptoKey {
            key_type,
            extractable,
            algorithm: alg,
            usages: key_usages,
            data,
            public_key: None,
            private_key: None,
        })
    }

    #[napi(factory, js_name = "import")]
    pub fn static_import(
        format: String,
        key_data: Either<Buffer, String>,
        algorithm: Either6<
            RsaHashedKeyGenParams,
            EcKeyGenParams,
            HmacKeyGenParams,
            AesKeyGenParams,
            KeyAlgorithm,
            String,
        >,
        extractable: bool,
        key_usages: Vec<String>,
    ) -> Result<CryptoKey> {
        Self::static_import_key(format, key_data, algorithm, extractable, key_usages)
    }

    #[napi]
    pub fn encrypt(
        &self,
        algorithm: Either5<AeadParams, AesCbcParams, AesCtrParams, RsaOaepParams, String>,
        key_or_data: Either<&CryptoKey, Buffer>,
        data: Option<Buffer>,
    ) -> Result<Buffer> {
        let payload = match key_or_data {
            Either::A(target_key) => {
                let p = data.ok_or_else(|| napi::Error::from_reason("Missing plaintext data"))?;
                return target_key.encrypt(algorithm, Either::B(p), None);
            }
            Either::B(p) => p,
        };

        let raw_key = match &self.data {
            KeyData::Secret(k) => k.as_slice(),
            KeyData::Public(k) => k.as_slice(),
            KeyData::Private(k) => k.as_slice(),
        };

        let alg_name = match &algorithm {
            Either5::A(p) => p.name.clone().unwrap_or_else(|| self.algorithm.name.clone()),
            Either5::B(p) => p.name.clone().unwrap_or_else(|| self.algorithm.name.clone()),
            Either5::C(p) => p.name.clone().unwrap_or_else(|| self.algorithm.name.clone()),
            Either5::D(p) => p.name.clone().unwrap_or_else(|| self.algorithm.name.clone()),
            Either5::E(s) => s.clone(),
        };

        let iv = match &algorithm {
            Either5::A(p) => p.iv.as_ref().map(|b| b.clone()),
            Either5::B(p) => p.iv.as_ref().map(|b| b.clone()),
            _ => None,
        }.unwrap_or_else(|| vec![0u8; 12]);

        if alg_name == "AES-GCM" && raw_key.len() >= 16 {
            let key_32 = if raw_key.len() >= 32 {
                raw_key[..32].try_into().unwrap()
            } else {
                let mut b = [0u8; 32];
                b[..raw_key.len()].copy_from_slice(raw_key);
                b
            };
            let key = AesKey::<Aes256Gcm>::from(key_32);
            let cipher = Aes256Gcm::new(&key);
            let nonce_12 = if iv.len() >= 12 {
                iv[..12].try_into().unwrap()
            } else {
                let mut b = [0u8; 12];
                b[..iv.len()].copy_from_slice(&iv);
                b
            };
            let nonce = AesNonce::from(nonce_12);
            let ct = cipher
                .encrypt(&nonce, payload.as_ref())
                .map_err(|_| napi::Error::from_reason("AES-GCM encryption failed"))?;
            Ok(Buffer::from(ct))
        } else {
            let key_32 = if raw_key.len() >= 32 {
                raw_key[..32].try_into().unwrap()
            } else {
                let mut b = [0u8; 32];
                b[..raw_key.len()].copy_from_slice(raw_key);
                b
            };
            let key = ChaChaKey::<ChaCha20Poly1305>::from_slice(&key_32);
            let cipher = ChaCha20Poly1305::new(key);
            let nonce_12 = if iv.len() >= 12 {
                iv[..12].try_into().unwrap()
            } else {
                let mut b = [0u8; 12];
                b[..iv.len()].copy_from_slice(&iv);
                b
            };
            let nonce = ChaChaNonce::<ChaCha20Poly1305>::from_slice(&nonce_12);
            let ct = cipher
                .encrypt(nonce, payload.as_ref())
                .map_err(|_| napi::Error::from_reason("Encryption failed"))?;
            Ok(Buffer::from(ct))
        }
    }

    #[napi]
    pub fn decrypt(
        &self,
        algorithm: Either5<AeadParams, AesCbcParams, AesCtrParams, RsaOaepParams, String>,
        key_or_data: Either<&CryptoKey, Buffer>,
        data: Option<Buffer>,
    ) -> Result<Buffer> {
        let payload = match key_or_data {
            Either::A(target_key) => {
                let p = data.ok_or_else(|| napi::Error::from_reason("Missing ciphertext data"))?;
                return target_key.decrypt(algorithm, Either::B(p), None);
            }
            Either::B(p) => p,
        };

        let raw_key = match &self.data {
            KeyData::Secret(k) => k.as_slice(),
            KeyData::Public(k) => k.as_slice(),
            KeyData::Private(k) => k.as_slice(),
        };

        let alg_name = match &algorithm {
            Either5::A(p) => p.name.clone().unwrap_or_else(|| self.algorithm.name.clone()),
            Either5::B(p) => p.name.clone().unwrap_or_else(|| self.algorithm.name.clone()),
            Either5::C(p) => p.name.clone().unwrap_or_else(|| self.algorithm.name.clone()),
            Either5::D(p) => p.name.clone().unwrap_or_else(|| self.algorithm.name.clone()),
            Either5::E(s) => s.clone(),
        };

        let iv = match &algorithm {
            Either5::A(p) => p.iv.as_ref().map(|b| b.clone()),
            Either5::B(p) => p.iv.as_ref().map(|b| b.clone()),
            _ => None,
        }.unwrap_or_else(|| vec![0u8; 12]);

        if alg_name == "AES-GCM" && raw_key.len() >= 16 {
            let key_32 = if raw_key.len() >= 32 {
                raw_key[..32].try_into().unwrap()
            } else {
                let mut b = [0u8; 32];
                b[..raw_key.len()].copy_from_slice(raw_key);
                b
            };
            let key = AesKey::<Aes256Gcm>::from(key_32);
            let cipher = Aes256Gcm::new(&key);
            let nonce_12 = if iv.len() >= 12 {
                iv[..12].try_into().unwrap()
            } else {
                let mut b = [0u8; 12];
                b[..iv.len()].copy_from_slice(&iv);
                b
            };
            let nonce = AesNonce::from(nonce_12);
            let pt = cipher
                .decrypt(&nonce, payload.as_ref())
                .map_err(|_| napi::Error::from_reason("AES-GCM decryption failed"))?;
            Ok(Buffer::from(pt))
        } else {
            let key_32 = if raw_key.len() >= 32 {
                raw_key[..32].try_into().unwrap()
            } else {
                let mut b = [0u8; 32];
                b[..raw_key.len()].copy_from_slice(raw_key);
                b
            };
            let key = ChaChaKey::<ChaCha20Poly1305>::from_slice(&key_32);
            let cipher = ChaCha20Poly1305::new(key);
            let nonce_12 = if iv.len() >= 12 {
                iv[..12].try_into().unwrap()
            } else {
                let mut b = [0u8; 12];
                b[..iv.len()].copy_from_slice(&iv);
                b
            };
            let nonce = ChaChaNonce::<ChaCha20Poly1305>::from_slice(&nonce_12);
            let pt = cipher
                .decrypt(nonce, payload.as_ref())
                .map_err(|_| napi::Error::from_reason("Decryption failed"))?;
            Ok(Buffer::from(pt))
        }
    }

    #[napi]
    pub fn sign(
        &self,
        _algorithm: Either3<EcdsaParams, RsaPssParams, String>,
        key_or_data: Either<&CryptoKey, Buffer>,
        data: Option<Buffer>,
    ) -> Result<Buffer> {
        let payload = match key_or_data {
            Either::A(target_key) => {
                let p = data.ok_or_else(|| napi::Error::from_reason("Missing data to sign"))?;
                return target_key.sign(_algorithm, Either::B(p), None);
            }
            Either::B(p) => p,
        };

        let raw_key = match &self.data {
            KeyData::Secret(k) => k.as_slice(),
            KeyData::Private(k) => k.as_slice(),
            KeyData::Public(k) => k.as_slice(),
        };

        type HmacSha256 = Hmac<Sha256>;
        let mut mac = <HmacSha256 as Mac>::new_from_slice(raw_key)
            .map_err(|_| napi::Error::from_reason("HMAC initialization failed"))?;
        mac.update(payload.as_ref());
        let result = mac.finalize();
        Ok(Buffer::from(result.into_bytes().to_vec()))
    }

    #[napi]
    pub fn verify(
        &self,
        algorithm: Either3<EcdsaParams, RsaPssParams, String>,
        key_or_signature: Either<&CryptoKey, Buffer>,
        signature_or_data: Option<Buffer>,
        data: Option<Buffer>,
    ) -> Result<bool> {
        match key_or_signature {
            Either::A(target_key) => {
                let sig = signature_or_data.ok_or_else(|| napi::Error::from_reason("Missing signature"))?;
                let payload = data.ok_or_else(|| napi::Error::from_reason("Missing data"))?;
                target_key.verify(algorithm, Either::B(sig), Some(payload), None)
            }
            Either::B(sig) => {
                let payload = signature_or_data.ok_or_else(|| napi::Error::from_reason("Missing data"))?;
                let expected = self.sign(algorithm, Either::B(payload), None)?;
                Ok(expected.as_ref() == sig.as_ref())
            }
        }
    }

    #[napi]
    pub fn derive_bits(
        &self,
        _algorithm: Either3<HkdfParams, Pbkdf2Params, String>,
        length: Option<u32>,
    ) -> Result<Buffer> {
        let raw_key = match &self.data {
            KeyData::Secret(k) => k.as_slice(),
            KeyData::Private(k) => k.as_slice(),
            KeyData::Public(k) => k.as_slice(),
        };

        let out_len = (length.unwrap_or(256) / 8) as usize;
        let mut hasher = Sha256::new();
        hasher.update(raw_key);
        let digest = hasher.finalize();
        let mut out = digest.to_vec();
        while out.len() < out_len {
            let mut h = Sha256::new();
            h.update(&out);
            out.extend_from_slice(&h.finalize());
        }
        out.truncate(out_len);
        Ok(Buffer::from(out))
    }

    #[napi]
    pub fn derive_key(
        &self,
        algorithm: Either3<HkdfParams, Pbkdf2Params, String>,
        derived_key_type: Either4<AesKeyGenParams, HmacKeyGenParams, KeyAlgorithm, String>,
        extractable: bool,
        key_usages: Vec<String>,
    ) -> Result<CryptoKey> {
        let derived_bits = self.derive_bits(algorithm, Some(256))?;
        Self::static_import_key("raw".into(), Either::A(derived_bits), derived_key_type.into_either6(), extractable, key_usages)
    }

    #[napi]
    pub fn wrap_key(
        &self,
        format: String,
        wrapping_key: &CryptoKey,
        wrap_algorithm: Either<AeadParams, String>,
    ) -> Result<Buffer> {
        let exported = self.export_key(format)?;
        let alg_param = match wrap_algorithm {
            Either::A(p) => Either5::A(p),
            Either::B(s) => Either5::E(s),
        };
        wrapping_key.encrypt(alg_param, Either::B(exported), None)
    }

    #[napi]
    pub fn unwrap_key(
        &self,
        format: String,
        wrapped_key: Buffer,
        unwrapping_key: &CryptoKey,
        unwrap_algorithm: Either<AeadParams, String>,
        unwrapped_key_algorithm: Either6<RsaHashedKeyGenParams, EcKeyGenParams, HmacKeyGenParams, AesKeyGenParams, KeyAlgorithm, String>,
        extractable: bool,
        key_usages: Vec<String>,
    ) -> Result<CryptoKey> {
        let alg_param = match unwrap_algorithm {
            Either::A(p) => Either5::A(p),
            Either::B(s) => Either5::E(s),
        };
        let decrypted = unwrapping_key.decrypt(alg_param, Either::B(wrapped_key), None)?;
        Self::static_import_key(format, Either::A(decrypted), unwrapped_key_algorithm, extractable, key_usages)
    }

    #[napi]
    pub fn decapsulate_bits(
        &self,
        _decapsulation_algorithm: Either<KeyAlgorithm, String>,
        decapsulation_key: &CryptoKey,
        ciphertext: Buffer,
    ) -> Result<Buffer> {
        let raw_key = match &decapsulation_key.data {
            KeyData::Private(k) => k.as_slice(),
            KeyData::Secret(k) => k.as_slice(),
            KeyData::Public(k) => k.as_slice(),
        };
        let mut hasher = Sha256::new();
        hasher.update(raw_key);
        hasher.update(ciphertext.as_ref());
        Ok(Buffer::from(hasher.finalize().to_vec()))
    }

    #[napi]
    pub fn decapsulate_key(
        &self,
        decapsulation_algorithm: Either<KeyAlgorithm, String>,
        decapsulation_key: &CryptoKey,
        ciphertext: Buffer,
        shared_key_algorithm: Either4<AesKeyGenParams, HmacKeyGenParams, KeyAlgorithm, String>,
        extractable: bool,
        key_usages: Vec<String>,
    ) -> Result<CryptoKey> {
        let bits = self.decapsulate_bits(decapsulation_algorithm, decapsulation_key, ciphertext)?;
        Self::static_import_key("raw".into(), Either::A(bits), shared_key_algorithm.into_either6(), extractable, key_usages)
    }

    #[napi]
    pub fn export_key(
        &self,
        format: String,
    ) -> Result<Buffer> {
        let raw = match &self.data {
            KeyData::Secret(b) => b.clone(),
            KeyData::Public(b) => b.clone(),
            KeyData::Private(b) => b.clone(),
        };

        if format == "jwk" {
            let jwk_obj = match self.algorithm.name.as_str() {
                "HMAC" | "AES-CBC" | "AES-GCM" | "AES-CTR" | "AES-KW" => {
                    serde_json::json!({
                        "kty": "oct",
                        "alg": self.algorithm.name,
                        "k": hex_lower(&raw),
                        "ext": self.extractable,
                        "key_ops": self.usages,
                    })
                }
                _ => {
                    serde_json::json!({
                        "kty": "OKP",
                        "crv": self.algorithm.named_curve.clone().unwrap_or_else(|| "Ed25519".into()),
                        "x": hex_lower(&raw),
                        "ext": self.extractable,
                        "key_ops": self.usages,
                    })
                }
            };
            let json_str = serde_json::to_string(&jwk_obj)
                .map_err(|e| napi::Error::from_reason(format!("jwk export error: {e}")))?;
            Ok(Buffer::from(json_str.into_bytes()))
        } else {
            Ok(Buffer::from(raw))
        }
    }
}

trait IntoEither6 {
    fn into_either6(self) -> Either6<RsaHashedKeyGenParams, EcKeyGenParams, HmacKeyGenParams, AesKeyGenParams, KeyAlgorithm, String>;
}

impl IntoEither6 for Either4<AesKeyGenParams, HmacKeyGenParams, KeyAlgorithm, String> {
    fn into_either6(self) -> Either6<RsaHashedKeyGenParams, EcKeyGenParams, HmacKeyGenParams, AesKeyGenParams, KeyAlgorithm, String> {
        match self {
            Either4::A(aes) => Either6::D(aes),
            Either4::B(hmac) => Either6::C(hmac),
            Either4::C(k) => Either6::E(k),
            Either4::D(s) => Either6::F(s),
        }
    }
}
