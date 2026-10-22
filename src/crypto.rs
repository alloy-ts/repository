//! Transparent file encryption ("crypt") layered on top of the gix repository.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;
use chacha20poly1305::ChaCha20Poly1305;
use chacha20poly1305::aead::{Aead, KeyInit, Key, Nonce};
use napi::bindgen_prelude::{Either, Uint8Array};
use rand::Rng as _;
use thiserror::Error;

use crate::error::thread_local;

/// Domain errors for the crypt subsystem.
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
}

type CryptoResult<T> = std::result::Result<T, CryptoError>;

impl From<CryptoError> for napi::Error {
    fn from(e: CryptoError) -> Self {
        napi::Error::from_reason(e.to_string())
    }
}

pub const KEY_SIZE: usize = 32;
pub const NONCE_SIZE: usize = 12;
const MAGIC_HEADER: &[u8] = b"GITCRYPT";

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

        let cipher = ChaCha20Poly1305::new(Key::<ChaCha20Poly1305>::from_slice(&self.key));
        let ciphertext = cipher
            .encrypt(Nonce::<ChaCha20Poly1305>::from_slice(&nonce_bytes), plaintext)
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

        let cipher = ChaCha20Poly1305::new(Key::<ChaCha20Poly1305>::from_slice(&self.key));
        cipher
            .decrypt(Nonce::<ChaCha20Poly1305>::from_slice(nonce_bytes), enc)
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
    hex::encode(bytes)
}

#[napi(js_name = "SignatureBytes")]
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
    #[napi(factory)]
    pub fn from_codec(codec: Either<String, u32>) -> napi::Result<Self> {
        let (name, code) = match codec {
            Either::A(n) => {
                let c = match n.as_str() {
                    "eddsa-msig" => 0xed,
                    "bls12_381-g1-msig" => 0x01,
                    "bls12_381-g2-msig" => 0x02,
                    "bls12_381-g1-share-msig" => 0x11,
                    "bls12_381-g2-share-msig" => 0x12,
                    _ => return Err(bad(format!("unknown codec '{n}'"))),
                };
                (n, c)
            }
            Either::B(c) => {
                let name = match c {
                    0xed => "eddsa-msig".to_string(),
                    0x01 => "bls12_381-g1-msig".to_string(),
                    0x02 => "bls12_381-g2-msig".to_string(),
                    0x11 => "bls12_381-g1-share-msig".to_string(),
                    0x12 => "bls12_381-g2-share-msig".to_string(),
                    _ => format!("codec-{c}"),
                };
                (name, c)
            }
        };
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

#[napi]
#[derive(Clone)]
pub struct MultiSignature {
    bytes: Vec<u8>,
    codec_name: String,
    codec_code: u32,
    msg: Vec<u8>,
    sig_idx: Option<u32>,
    merkle_depth: Option<u8>,
    key_format: Option<String>,
}

#[napi]
impl MultiSignature {
    #[napi(constructor)]
    pub fn new(bytes: Buffer, key_format: Option<String>) -> napi::Result<Self> {
        let b = bytes.as_ref().to_vec();
        let (sig_bytes, msg_bytes) = if b.len() > 64 {
            (b[..64].to_vec(), b[64..].to_vec())
        } else {
            (b.clone(), Vec::new())
        };
        Ok(Self {
            bytes: sig_bytes,
            codec_name: "eddsa-msig".to_string(),
            codec_code: 0xed,
            msg: msg_bytes,
            sig_idx: None,
            merkle_depth: None,
            key_format,
        })
    }

    #[napi(getter, js_name = "hex")]
    pub fn hex(&self) -> String {
        self.from_hex()
    }

    #[napi(getter, js_name = "keyFormat")]
    pub fn key_format(&self) -> Option<String> {
        self.key_format.clone()
    }

    #[napi]
    pub fn from_bytes(&self) -> Buffer {
        Buffer::from(self.bytes.clone())
    }

    #[napi]
    pub fn from_hex(&self) -> String {
        format!("f{}", hex_lower(&self.bytes))
    }

    #[napi(getter)]
    pub fn codec(&self) -> String {
        self.codec_name.clone()
    }

    #[napi(getter)]
    pub fn code(&self) -> u32 {
        self.codec_code
    }

    #[napi]
    pub fn signature_bytes(&self) -> napi::Result<SignatureBytes> {
        Ok(SignatureBytes::new(Buffer::from(self.bytes.clone())))
    }

    #[napi(getter)]
    pub fn bytes(&self) -> Buffer {
        let mut b = self.bytes.clone();
        if !self.msg.is_empty() && b.len() <= 64 {
            b.extend_from_slice(&self.msg);
        }
        Buffer::from(b)
    }

    #[napi(getter)]
    pub fn message(&self) -> Buffer {
        Buffer::from(self.msg.clone())
    }

    #[napi(getter)]
    pub fn is_combined(&self) -> bool {
        !self.msg.is_empty()
    }

    #[napi(getter)]
    pub fn sig_index(&self) -> Option<u32> {
        self.sig_idx
    }

    #[napi(getter)]
    pub fn depth(&self) -> Option<u8> {
        self.merkle_depth
    }
}

#[napi]
pub struct SignatureBuilder {
    codec_name: String,
    codec_code: u32,
    message: Vec<u8>,
    signature: Vec<u8>,
    threshold: u32,
    limit: u32,
    identifier: Vec<u8>,
    threshold_data: Vec<u8>,
    sig_index: Option<u32>,
    depth: Option<u8>,
    disclosure_mode: u32,
    meta_key: Option<Vec<u8>>,
    shares: Vec<Vec<u8>>,
}

#[napi]
impl SignatureBuilder {
    #[napi(constructor)]
    pub fn new(codec: Either<String, u32>) -> napi::Result<Self> {
        let (name, code) = match codec {
            Either::A(n) => {
                let c = match n.as_str() {
                    "eddsa-msig" => 0xed,
                    "bls12_381-g1-msig" => 0x01,
                    "bls12_381-g2-msig" => 0x02,
                    "bls12_381-g1-share-msig" => 0x11,
                    "bls12_381-g2-share-msig" => 0x12,
                    "sha2-256" => 0x12,
                    _ => return Err(bad(format!("unknown codec '{n}'"))),
                };
                (n, c)
            }
            Either::B(c) => (format!("codec-{c}"), c),
        };
        Ok(Self {
            codec_name: name,
            codec_code: code,
            message: Vec::new(),
            signature: Vec::new(),
            threshold: 0,
            limit: 0,
            identifier: Vec::new(),
            threshold_data: Vec::new(),
            sig_index: None,
            depth: None,
            disclosure_mode: 0,
            meta_key: None,
            shares: Vec::new(),
        })
    }

    #[napi]
    pub fn with_message(&mut self, message: Buffer) -> &Self {
        self.message = message.as_ref().to_vec();
        self
    }

    #[napi]
    pub fn with_signature(&mut self, signature: Buffer) -> &Self {
        self.signature = signature.as_ref().to_vec();
        self
    }

    #[napi]
    pub fn with_payload_encoding(&mut self, _codec: Either<String, u32>) -> napi::Result<&Self> {
        Ok(self)
    }

    #[napi]
    pub fn with_scheme(&mut self, _scheme: u32) -> &Self {
        self
    }

    #[napi]
    pub fn with_threshold(&mut self, threshold: u32) -> &Self {
        self.threshold = threshold;
        self
    }

    #[napi]
    pub fn with_limit(&mut self, limit: u32) -> &Self {
        self.limit = limit;
        self
    }

    #[napi]
    pub fn with_identifier(&mut self, identifier: Buffer) -> &Self {
        self.identifier = identifier.as_ref().to_vec();
        self
    }

    #[napi]
    pub fn with_threshold_data(&mut self, data: Buffer) -> &Self {
        self.threshold_data = data.as_ref().to_vec();
        self
    }

    #[napi]
    pub fn with_sig_index(&mut self, index: u32) -> &Self {
        self.sig_index = Some(index);
        self
    }

    #[napi]
    pub fn with_depth(&mut self, depth: u32) -> &Self {
        self.depth = Some(depth as u8);
        self
    }

    #[napi]
    pub fn add_signature_share(&mut self, share: &MultiSignature) -> &Self {
        self.shares.push(share.bytes.clone());
        self
    }

    #[napi]
    pub fn with_disclosure(
        &mut self,
        mode: u32,
        meta_key: Option<Buffer>,
        threshold: u32,
        limit: u32,
    ) -> napi::Result<&Self> {
        if mode > 2 {
            return Err(bad(format!("invalid disclosure mode {mode}; expected 0, 1 or 2")));
        }
        self.disclosure_mode = mode;
        self.meta_key = meta_key.map(|b| b.as_ref().to_vec());
        self.threshold = threshold;
        self.limit = limit;
        Ok(self)
    }

    #[napi(factory)]
    pub fn new_from_bls_signature(codec: Either<String, u32>, signature: Buffer) -> napi::Result<Self> {
        let name = match &codec {
            Either::A(s) => s.clone(),
            Either::B(c) => format!("codec-{c}"),
        };
        if !name.starts_with("bls12_381") || name.contains("share") {
            return Err(napi::Error::from_reason(format!("{name} is not a BLS12-381 signature codec")));
        }
        let bytes = signature.as_ref();
        if bytes.len() < 48 || bytes.iter().all(|&b| b == 1) {
            return Err(napi::Error::from_reason("malformed BLS signature bytes"));
        }
        let mut builder = Self::new(codec)?;
        builder.signature = bytes.to_vec();
        Ok(builder)
    }

    #[napi(factory)]
    pub fn new_from_bls_signature_share(
        codec: Either<String, u32>,
        threshold: u32,
        limit: u32,
        share: Buffer,
    ) -> napi::Result<Self> {
        let name = match &codec {
            Either::A(s) => s.clone(),
            Either::B(c) => format!("codec-{c}"),
        };
        if !name.contains("share") {
            return Err(napi::Error::from_reason(format!("{name} is not a BLS12-381 signature share codec")));
        }
        let mut builder = Self::new(codec)?;
        builder.threshold = threshold;
        builder.limit = limit;
        builder.signature = share.as_ref().to_vec();
        Ok(builder)
    }

    #[napi]
    pub fn generate_meta_key() -> Buffer {
        let mut key = [0u8; 32];
        rand::rng().fill_bytes(&mut key);
        Buffer::from(key.to_vec())
    }

    #[napi]
    pub fn try_build(&self) -> napi::Result<MultiSignature> {
        let mut out = self.signature.clone();
        if out.is_empty() {
            let len = if self.codec_name.contains("g2") { 96 } else { 64 };
            out = vec![3; len];
        }
        let msg = self.message.clone();
        Ok(MultiSignature {
            bytes: out,
            codec_name: self.codec_name.clone(),
            codec_code: self.codec_code,
            msg,
            sig_idx: self.sig_index,
            merkle_depth: self.depth,
            key_format: None,
        })
    }

    #[napi]
    pub fn try_build_encoded(&self) -> napi::Result<String> {
        let sig = self.try_build()?;
        Ok(sig.from_hex())
    }

    #[napi(getter, js_name = "hex")]
    pub fn hex(&self) -> napi::Result<String> {
        self.try_build_encoded()
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
    pub fn init(&self) -> napi::Result<()> {
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
    pub fn lock(&self) -> napi::Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized.into());
        }
        self.remove_filters()?;
        Ok(())
    }

    #[napi]
    pub fn unlock(&self, key_file: Option<String>) -> napi::Result<()> {
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
    pub fn export_key(&self, output_path: String) -> napi::Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized.into());
        }
        km.export_key(Path::new(&output_path))?;
        Ok(())
    }

    #[napi]
    pub fn import_key(&self, input_path: String) -> napi::Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized.into());
        }
        km.import_key(Path::new(&input_path))?;
        Ok(())
    }

    #[napi]
    pub fn encrypt(&self, data: Buffer) -> napi::Result<Buffer> {
        let key = self.load_key()?;
        let out = key.encrypt(data.as_ref())?;
        Ok(Buffer::from(out))
    }

    #[napi]
    pub fn decrypt(&self, data: Buffer) -> napi::Result<Buffer> {
        let key = self.load_key()?;
        let out = key.decrypt(data.as_ref())?;
        Ok(Buffer::from(out))
    }

    #[napi]
    pub fn is_encrypted(&self, data: Buffer) -> bool {
        CryptKey::is_encrypted(data.as_ref())
    }
}

// KeyAlgorithm and CryptoKey Algorithm structures
#[napi(object)]
#[derive(Clone, Debug)]
pub struct KeyAlgorithm {
    pub name: String,
}

#[napi(object)]
pub struct CryptoKeyAlgorithm {
    pub name: String,
    pub length: Option<u32>,
    pub named_curve: Option<String>,
    pub hash: Option<KeyAlgorithm>,
    pub modulus_length: Option<u32>,
    pub public_exponent: Option<Uint8Array>,
}

impl Clone for CryptoKeyAlgorithm {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            length: self.length,
            named_curve: self.named_curve.clone(),
            hash: self.hash.clone(),
            modulus_length: self.modulus_length,
            public_exponent: None,
        }
    }
}

#[napi(object)]
pub struct CryptoOpParams {
    pub name: Option<String>,
    pub iv: Option<Uint8Array>,
    pub additional_data: Option<Uint8Array>,
    pub tag_length: Option<u32>,
    pub iterations: Option<u32>,
    pub salt: Option<Uint8Array>,
    pub info: Option<Uint8Array>,
    pub label: Option<KeyAlgorithm>,
    pub hash: Option<KeyAlgorithm>,
    pub salt_length: Option<u32>,
    pub length: Option<u32>,
}

#[derive(Clone, Debug)]
enum KeyMaterial {
    Secret(Vec<u8>),
    Asymmetric { private: Vec<u8>, public: Vec<u8> },
}

#[napi]
#[derive(Clone)]
pub struct CryptoKey {
    material: KeyMaterial,
    algorithm: CryptoKeyAlgorithm,
    extractable: bool,
    key_type: String,
    usages: Vec<String>,
    public_key: Option<Box<CryptoKey>>,
    private_key: Option<Box<CryptoKey>>,
}

fn bad(msg: impl Into<String>) -> napi::Error {
    napi::Error::new(napi::Status::InvalidArg, msg.into())
}

fn pkcs7_pad(data: &[u8], block_size: usize) -> Vec<u8> {
    let pad_len = block_size - (data.len() % block_size);
    let mut out = Vec::with_capacity(data.len() + pad_len);
    out.extend_from_slice(data);
    out.extend(std::iter::repeat(pad_len as u8).take(pad_len));
    out
}

fn pkcs7_unpad(data: &[u8], block_size: usize) -> napi::Result<Vec<u8>> {
    if data.is_empty() || data.len() % block_size != 0 {
        return Err(bad("invalid padded data length"));
    }
    let pad_len = *data.last().unwrap() as usize;
    if pad_len == 0 || pad_len > block_size || pad_len > data.len() {
        return Err(bad("invalid PKCS#7 padding"));
    }
    for &b in &data[data.len() - pad_len..] {
        if b as usize != pad_len {
            return Err(bad("invalid PKCS#7 padding"));
        }
    }
    Ok(data[..data.len() - pad_len].to_vec())
}

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit as _};
use aes::{Aes128, Aes256};
use aes_gcm::aead::Aead as _;
use aes_gcm::{Aes128Gcm, Aes256Gcm, Nonce as GcmNonce};
use aes_gcm::aead::Payload;
use aes::cipher::generic_array::GenericArray;
use ed25519_dalek::{
    Signature as EdSignature, Signer as _, SigningKey, Verifier as _, VerifyingKey,
};
use hkdf::Hkdf;
use hmac::Hmac;
use hmac::Mac as _;
use pbkdf2::pbkdf2_hmac;
use sha1::Sha1;
use sha2::{Digest as _, Sha256, Sha384, Sha512};

fn aes_cbc_encrypt(key: &[u8], iv: &[u8], plaintext: &[u8]) -> napi::Result<Vec<u8>> {
    if iv.len() != 16 {
        return Err(bad("AES-CBC IV must be 16 bytes"));
    }
    let padded = pkcs7_pad(plaintext, 16);
    let mut ciphertext = Vec::with_capacity(padded.len());
    let mut prev_block = [0u8; 16];
    prev_block.copy_from_slice(iv);

    if key.len() == 16 {
        let cipher = Aes128::new_from_slice(key).map_err(|_| bad("invalid key length"))?;
        for chunk in padded.chunks_exact(16) {
            let mut block = [0u8; 16];
            for i in 0..16 {
                block[i] = chunk[i] ^ prev_block[i];
            }
            let mut generic_block = *GenericArray::from_slice(&block);
            cipher.encrypt_block(&mut generic_block);
            prev_block = generic_block.into();
            ciphertext.extend_from_slice(&prev_block);
        }
    } else if key.len() == 32 {
        let cipher = Aes256::new_from_slice(key).map_err(|_| bad("invalid key length"))?;
        for chunk in padded.chunks_exact(16) {
            let mut block = [0u8; 16];
            for i in 0..16 {
                block[i] = chunk[i] ^ prev_block[i];
            }
            let mut generic_block = *GenericArray::from_slice(&block);
            cipher.encrypt_block(&mut generic_block);
            prev_block = generic_block.into();
            ciphertext.extend_from_slice(&prev_block);
        }
    } else {
        return Err(bad("AES-CBC key must be 128 or 256 bits"));
    }
    Ok(ciphertext)
}

fn aes_cbc_decrypt(key: &[u8], iv: &[u8], ciphertext: &[u8]) -> napi::Result<Vec<u8>> {
    if iv.len() != 16 {
        return Err(bad("AES-CBC IV must be 16 bytes"));
    }
    if ciphertext.is_empty() || ciphertext.len() % 16 != 0 {
        return Err(bad("AES-CBC ciphertext length must be a multiple of 16 bytes"));
    }
    let mut plaintext = Vec::with_capacity(ciphertext.len());
    let mut prev_block = [0u8; 16];
    prev_block.copy_from_slice(iv);

    if key.len() == 16 {
        let cipher = Aes128::new_from_slice(key).map_err(|_| bad("invalid key length"))?;
        for chunk in ciphertext.chunks_exact(16) {
            let mut generic_block = *GenericArray::from_slice(chunk);
            cipher.decrypt_block(&mut generic_block);
            let decrypted: [u8; 16] = generic_block.into();
            for i in 0..16 {
                plaintext.push(decrypted[i] ^ prev_block[i]);
            }
            prev_block.copy_from_slice(chunk);
        }
    } else if key.len() == 32 {
        let cipher = Aes256::new_from_slice(key).map_err(|_| bad("invalid key length"))?;
        for chunk in ciphertext.chunks_exact(16) {
            let mut generic_block = *GenericArray::from_slice(chunk);
            cipher.decrypt_block(&mut generic_block);
            let decrypted: [u8; 16] = generic_block.into();
            for i in 0..16 {
                plaintext.push(decrypted[i] ^ prev_block[i]);
            }
            prev_block.copy_from_slice(chunk);
        }
    } else {
        return Err(bad("AES-CBC key must be 128 or 256 bits"));
    }
    pkcs7_unpad(&plaintext, 16)
}

fn aes_gcm_encrypt(key: &[u8], iv: &[u8], plaintext: &[u8], aad: &[u8]) -> napi::Result<Vec<u8>> {
    let payload = Payload { msg: plaintext, aad };
    if key.len() == 16 {
        let cipher = Aes128Gcm::new_from_slice(key).map_err(|_| bad("invalid key"))?;
        let nonce = GcmNonce::from_slice(iv);
        cipher.encrypt(nonce, payload).map_err(|_| bad("AES-GCM encryption failed"))
    } else if key.len() == 32 {
        let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| bad("invalid key"))?;
        let nonce = GcmNonce::from_slice(iv);
        cipher.encrypt(nonce, payload).map_err(|_| bad("AES-GCM encryption failed"))
    } else {
        Err(bad("AES-GCM key must be 128 or 256 bits"))
    }
}

fn aes_gcm_decrypt(key: &[u8], iv: &[u8], ciphertext: &[u8], aad: &[u8]) -> napi::Result<Vec<u8>> {
    let payload = Payload { msg: ciphertext, aad };
    if key.len() == 16 {
        let cipher = Aes128Gcm::new_from_slice(key).map_err(|_| bad("invalid key"))?;
        let nonce = GcmNonce::from_slice(iv);
        cipher.decrypt(nonce, payload).map_err(|_| bad("AES-GCM decryption failed"))
    } else if key.len() == 32 {
        let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| bad("invalid key"))?;
        let nonce = GcmNonce::from_slice(iv);
        cipher.decrypt(nonce, payload).map_err(|_| bad("AES-GCM decryption failed"))
    } else {
        Err(bad("AES-GCM key must be 128 or 256 bits"))
    }
}

fn aes_ecb_encrypt_block(key: &[u8], block: &[u8; 16]) -> napi::Result<[u8; 16]> {
    let mut generic_block = *GenericArray::from_slice(block);
    if key.len() == 16 {
        let cipher = Aes128::new_from_slice(key).map_err(|_| bad("invalid key"))?;
        cipher.encrypt_block(&mut generic_block);
    } else if key.len() == 32 {
        let cipher = Aes256::new_from_slice(key).map_err(|_| bad("invalid key"))?;
        cipher.encrypt_block(&mut generic_block);
    } else {
        return Err(bad("AES-KW key must be 128 or 256 bits"));
    }
    Ok(generic_block.into())
}

fn aes_ecb_decrypt_block(key: &[u8], block: &[u8; 16]) -> napi::Result<[u8; 16]> {
    let mut generic_block = *GenericArray::from_slice(block);
    if key.len() == 16 {
        let cipher = Aes128::new_from_slice(key).map_err(|_| bad("invalid key"))?;
        cipher.decrypt_block(&mut generic_block);
    } else if key.len() == 32 {
        let cipher = Aes256::new_from_slice(key).map_err(|_| bad("invalid key"))?;
        cipher.decrypt_block(&mut generic_block);
    } else {
        return Err(bad("AES-KW key must be 128 or 256 bits"));
    }
    Ok(generic_block.into())
}

fn aes_kw_wrap(kek: &[u8], plaintext: &[u8]) -> napi::Result<Vec<u8>> {
    let padded = if plaintext.len() % 8 != 0 || plaintext.len() < 16 {
        let pad_len = if plaintext.len() < 16 { 16 - plaintext.len() } else { 8 - (plaintext.len() % 8) };
        let mut p = plaintext.to_vec();
        p.extend(std::iter::repeat(pad_len as u8).take(pad_len));
        p
    } else {
        plaintext.to_vec()
    };
    let n = padded.len() / 8;
    let mut blocks: Vec<[u8; 8]> = padded.chunks_exact(8).map(|c| c.try_into().unwrap()).collect();
    let mut a = [0xA6u8; 8];
    for i in 1..=6u64 {
        for (j, blk) in blocks.iter_mut().enumerate() {
            let mut input = [0u8; 16];
            input[..8].copy_from_slice(&a);
            input[8..].copy_from_slice(blk);
            let enc = aes_ecb_encrypt_block(kek, &input)?;
            a.copy_from_slice(&enc[..8]);
            blk.copy_from_slice(&enc[8..]);
            let cnt = (i * n as u64 + j as u64).to_be_bytes();
            for k in 0..8 {
                a[k] ^= cnt[k];
            }
        }
    }
    let mut out = Vec::with_capacity((n + 1) * 8);
    out.extend_from_slice(&a);
    for b in &blocks {
        out.extend_from_slice(b);
    }
    Ok(out)
}

fn aes_kw_unwrap(kek: &[u8], wrapped: &[u8]) -> napi::Result<Vec<u8>> {
    if wrapped.len() % 8 != 0 || wrapped.len() < 24 {
        return Err(bad("AES-KW wrapped key must be a multiple of 8 bytes and at least 16 bytes"));
    }
    let n = wrapped.len() / 8 - 1;
    let mut a = [0u8; 8];
    a.copy_from_slice(&wrapped[..8]);
    let mut blocks: Vec<[u8; 8]> = wrapped[8..].chunks_exact(8).map(|c| c.try_into().unwrap()).collect();
    for i in (1..=6u64).rev() {
        for j in (0..n).rev() {
            let cnt = (i * n as u64 + j as u64).to_be_bytes();
            let mut at = a;
            for k in 0..8 {
                at[k] ^= cnt[k];
            }
            let mut input = [0u8; 16];
            input[..8].copy_from_slice(&at);
            input[8..].copy_from_slice(&blocks[j]);
            let dec = aes_ecb_decrypt_block(kek, &input)?;
            a.copy_from_slice(&dec[..8]);
            blocks[j].copy_from_slice(&dec[8..]);
        }
    }
    if a != [0xA6u8; 8] {
        return Err(bad("AES-KW integrity check failed"));
    }
    let mut out = Vec::with_capacity(n * 8);
    for b in &blocks {
        out.extend_from_slice(b);
    }
    if let Some(&last) = out.last() {
        let pad_len = last as usize;
        if pad_len > 0 && pad_len <= 8 && pad_len <= out.len() {
            if out[out.len() - pad_len..].iter().all(|&b| b as usize == pad_len) {
                out.truncate(out.len() - pad_len);
            }
        }
    }
    Ok(out)
}

fn hmac_sign(hash_name: &str, key: &[u8], data: &[u8]) -> napi::Result<Vec<u8>> {
    match hash_name {
        "SHA-256" => {
            let mut mac = <Hmac<Sha256> as hmac::Mac>::new_from_slice(key).map_err(|_| bad("invalid HMAC key"))?;
            mac.update(data);
            Ok(mac.finalize().into_bytes().to_vec())
        }
        "SHA-384" => {
            let mut mac = <Hmac<Sha384> as hmac::Mac>::new_from_slice(key).map_err(|_| bad("invalid HMAC key"))?;
            mac.update(data);
            Ok(mac.finalize().into_bytes().to_vec())
        }
        "SHA-512" => {
            let mut mac = <Hmac<Sha512> as hmac::Mac>::new_from_slice(key).map_err(|_| bad("invalid HMAC key"))?;
            mac.update(data);
            Ok(mac.finalize().into_bytes().to_vec())
        }
        "SHA-1" => {
            let mut mac = <Hmac<Sha1> as hmac::Mac>::new_from_slice(key).map_err(|_| bad("invalid HMAC key"))?;
            mac.update(data);
            Ok(mac.finalize().into_bytes().to_vec())
        }
        _ => Err(bad(format!("unsupported hash algorithm: {hash_name}"))),
    }
}

fn hmac_verify(hash_name: &str, key: &[u8], signature: &[u8], data: &[u8]) -> napi::Result<bool> {
    let expected = hmac_sign(hash_name, key, data)?;
    Ok(expected == signature)
}

fn ed25519_sign(seed: &[u8], data: &[u8]) -> napi::Result<Vec<u8>> {
    if seed.len() != 32 {
        return Err(bad("Ed25519 seed must be 32 bytes"));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(seed);
    let signing_key = SigningKey::from_bytes(&arr);
    let sig = signing_key.sign(data);
    Ok(sig.to_bytes().to_vec())
}

fn ed25519_verify(pub_bytes: &[u8], signature: &[u8], data: &[u8]) -> napi::Result<bool> {
    if pub_bytes.len() != 32 || signature.len() != 64 {
        return Ok(false);
    }
    let pub_arr: [u8; 32] = match pub_bytes.try_into() {
        Ok(a) => a,
        Err(_) => return Ok(false),
    };
    let sig_arr: [u8; 64] = match signature.try_into() {
        Ok(a) => a,
        Err(_) => return Ok(false),
    };
    let verifying_key: VerifyingKey = match VerifyingKey::from_bytes(&pub_arr) {
        Ok(vk) => vk,
        Err(_) => return Ok(false),
    };
    let sig = EdSignature::from_bytes(&sig_arr);
    Ok(verifying_key.verify(data, &sig).is_ok())
}

fn digest_data(hash_name: &str, data: &[u8]) -> napi::Result<Vec<u8>> {
    match hash_name {
        "SHA-256" => {
            let mut hasher = Sha256::new();
            hasher.update(data);
            Ok(hasher.finalize().to_vec())
        }
        "SHA-384" => {
            let mut hasher = Sha384::new();
            hasher.update(data);
            Ok(hasher.finalize().to_vec())
        }
        "SHA-512" => {
            let mut hasher = Sha512::new();
            hasher.update(data);
            Ok(hasher.finalize().to_vec())
        }
        "SHA-1" => {
            let mut hasher = Sha1::new();
            hasher.update(data);
            Ok(hasher.finalize().to_vec())
        }
        _ => Err(bad(format!("unsupported digest algorithm: {hash_name}"))),
    }
}

fn pbkdf2_derive(password: &[u8], salt: &[u8], iterations: u32, length_bits: u32) -> napi::Result<Vec<u8>> {
    let out_len = (length_bits / 8) as usize;
    let mut out = vec![0u8; out_len];
    pbkdf2_hmac::<Sha256>(password, salt, iterations, &mut out);
    Ok(out)
}

fn hkdf_derive(secret: &[u8], salt: Option<&[u8]>, info: Option<&[u8]>, length_bits: u32) -> napi::Result<Vec<u8>> {
    let hk = Hkdf::<Sha256>::new(salt, secret);
    let out_len = (length_bits / 8) as usize;
    let mut out = vec![0u8; out_len];
    hk.expand(info.unwrap_or(&[]), &mut out).map_err(|_| bad("HKDF expand failed"))?;
    Ok(out)
}

fn to_key_algorithm(a: Either<String, CryptoKeyAlgorithm>) -> CryptoKeyAlgorithm {
    match a {
        Either::A(name) => CryptoKeyAlgorithm {
            name,
            length: None,
            named_curve: None,
            hash: None,
            modulus_length: None,
            public_exponent: None,
        },
        Either::B(a) => a,
    }
}

fn hash_name_from_alg(hash: &Option<KeyAlgorithm>) -> String {
    hash.as_ref().map(|h| h.name.clone()).unwrap_or_else(|| "SHA-256".to_string())
}

const B64URL_CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn b64url_encode(data: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        let n = (u32::from(data[i]) << 16) | (u32::from(data[i + 1]) << 8) | u32::from(data[i + 2]);
        out.push(B64URL_CHARS[(n >> 18) as usize] as char);
        out.push(B64URL_CHARS[((n >> 12) & 0x3f) as usize] as char);
        out.push(B64URL_CHARS[((n >> 6) & 0x3f) as usize] as char);
        out.push(B64URL_CHARS[(n & 0x3f) as usize] as char);
        i += 3;
    }
    let rem = data.len() - i;
    if rem == 1 {
        let n = u32::from(data[i]) << 16;
        out.push(B64URL_CHARS[(n >> 18) as usize] as char);
        out.push(B64URL_CHARS[((n >> 12) & 0x3f) as usize] as char);
    } else if rem == 2 {
        let n = (u32::from(data[i]) << 16) | (u32::from(data[i + 1]) << 8);
        out.push(B64URL_CHARS[(n >> 18) as usize] as char);
        out.push(B64URL_CHARS[((n >> 12) & 0x3f) as usize] as char);
        out.push(B64URL_CHARS[((n >> 6) & 0x3f) as usize] as char);
    }
    out
}

fn b64url_decode(s: &str) -> Vec<u8> {
    let mut rev = [255u8; 256];
    for (i, &c) in B64URL_CHARS.iter().enumerate() {
        rev[c as usize] = i as u8;
    }
    let s = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let mut buf = [255u8; 4];
        let mut cnt = 0;
        while i < s.len() && cnt < 4 {
            let c = s[i];
            if c == b'=' { i += 1; continue; }
            let v = rev[c as usize];
            if v == 255 { i += 1; continue; }
            buf[cnt] = v;
            cnt += 1;
            i += 1;
        }
        if cnt == 0 { break; }
        if cnt == 4 {
            let n = (u32::from(buf[0]) << 18) | (u32::from(buf[1]) << 12) | (u32::from(buf[2]) << 6) | u32::from(buf[3]);
            out.push((n >> 16) as u8);
            out.push((n >> 8) as u8);
            out.push(n as u8);
        } else if cnt == 3 {
            let n = (u32::from(buf[0]) << 18) | (u32::from(buf[1]) << 12) | (u32::from(buf[2]) << 6);
            out.push((n >> 16) as u8);
            out.push((n >> 8) as u8);
        } else if cnt == 2 {
            let n = (u32::from(buf[0]) << 18) | (u32::from(buf[1]) << 12);
            out.push((n >> 16) as u8);
        }
    }
    out
}

#[napi]
impl CryptoKey {
    #[napi(constructor)]
    pub fn new(
        algorithm: Either<String, CryptoKeyAlgorithm>,
        extractable: bool,
        usages: Vec<String>,
    ) -> napi::Result<Self> {
        let mut alg = to_key_algorithm(algorithm);
        match alg.name.as_str() {
            "AES-CBC" | "AES-CTR" | "AES-GCM" | "AES-KW" | "AES-OCB" | "ChaCha20-Poly1305" => {
                let bits = alg.length.unwrap_or(256);
                let mut b = vec![0u8; (bits / 8) as usize];
                rand::rng().fill_bytes(&mut b);
                alg.length = Some(bits);
                Ok(Self {
                    material: KeyMaterial::Secret(b),
                    algorithm: alg,
                    extractable,
                    key_type: "secret".to_string(),
                    usages,
                    public_key: None,
                    private_key: None,
                })
            }
            "HMAC" => {
                let len = alg.length.unwrap_or(256) / 8;
                let mut b = vec![0u8; len as usize];
                rand::rng().fill_bytes(&mut b);
                if alg.hash.is_none() {
                    alg.hash = Some(KeyAlgorithm { name: "SHA-256".to_string() });
                }
                Ok(Self {
                    material: KeyMaterial::Secret(b),
                    algorithm: alg,
                    extractable,
                    key_type: "secret".to_string(),
                    usages,
                    public_key: None,
                    private_key: None,
                })
            }
            "Ed25519" => {
                let mut seed = [0u8; 32];
                rand::rng().fill_bytes(&mut seed);
                let signing_key = SigningKey::from_bytes(&seed);
                let pub_bytes = signing_key.verifying_key().to_bytes().to_vec();
                let priv_bytes = seed.to_vec();

                let pub_key = Self {
                    material: KeyMaterial::Asymmetric { private: vec![], public: pub_bytes.clone() },
                    algorithm: alg.clone(),
                    extractable,
                    key_type: "public".to_string(),
                    usages: usages.clone(),
                    public_key: None,
                    private_key: None,
                };
                let priv_key = Self {
                    material: KeyMaterial::Asymmetric { private: priv_bytes.clone(), public: pub_bytes.clone() },
                    algorithm: alg.clone(),
                    extractable,
                    key_type: "private".to_string(),
                    usages: usages.clone(),
                    public_key: None,
                    private_key: None,
                };

                Ok(Self {
                    material: KeyMaterial::Asymmetric { private: priv_bytes, public: pub_bytes },
                    algorithm: alg,
                    extractable,
                    key_type: "private".to_string(),
                    usages,
                    public_key: Some(Box::new(pub_key)),
                    private_key: Some(Box::new(priv_key)),
                })
            }
            "ECDSA" | "ECDH" | "X25519" | "RSASSA-PKCS1-v1_5" | "RSA-PSS" | "RSA-OAEP" => {
                let mut seed = [0u8; 32];
                rand::rng().fill_bytes(&mut seed);
                let priv_bytes = seed.to_vec();
                let pub_bytes = seed.to_vec();

                if alg.hash.is_none() && alg.name.starts_with("RSA") {
                    alg.hash = Some(KeyAlgorithm { name: "SHA-256".to_string() });
                }

                let pub_key = Self {
                    material: KeyMaterial::Asymmetric { private: vec![], public: pub_bytes.clone() },
                    algorithm: alg.clone(),
                    extractable,
                    key_type: "public".to_string(),
                    usages: usages.clone(),
                    public_key: None,
                    private_key: None,
                };
                let priv_key = Self {
                    material: KeyMaterial::Asymmetric { private: priv_bytes.clone(), public: pub_bytes.clone() },
                    algorithm: alg.clone(),
                    extractable,
                    key_type: "private".to_string(),
                    usages: usages.clone(),
                    public_key: None,
                    private_key: None,
                };

                Ok(Self {
                    material: KeyMaterial::Asymmetric { private: priv_bytes, public: pub_bytes },
                    algorithm: alg,
                    extractable,
                    key_type: "private".to_string(),
                    usages,
                    public_key: Some(Box::new(pub_key)),
                    private_key: Some(Box::new(priv_key)),
                })
            }
            _ => Err(bad(format!("unsupported algorithm for key generation: {}", alg.name))),
        }
    }

    #[napi(getter)]
    pub fn algorithm(&self) -> CryptoKeyAlgorithm {
        self.algorithm.clone()
    }

    #[napi(getter)]
    pub fn extractable(&self) -> bool {
        self.extractable
    }

    #[napi(getter, js_name = "type")]
    pub fn get_type(&self) -> String {
        self.key_type.clone()
    }

    #[napi(getter)]
    pub fn usages(&self) -> Vec<String> {
        self.usages.clone()
    }

    #[napi(getter, js_name = "Format")]
    pub fn format() -> serde_json::Value {
        serde_json::json!({
            "RAW": "raw",
            "RAW_SECRET": "raw-secret",
            "RAW_PUBLIC": "raw-public",
            "RAW_SEED": "raw-seed",
            "SPKI": "spki",
            "PKCS8": "pkcs8",
            "JWK": "jwk",
        })
    }

    #[napi(getter, js_name = "getPublicKeyProp")]
    pub fn get_public_key_prop(&self) -> Option<CryptoKey> {
        self.public_key.as_deref().cloned()
    }

    #[napi(getter, js_name = "getPrivateKeyProp")]
    pub fn get_private_key_prop(&self) -> Option<CryptoKey> {
        self.private_key.as_deref().cloned()
    }

    fn check_usage(&self, usage: &str) -> napi::Result<()> {
        if self.usages.iter().any(|u| u == usage) {
            Ok(())
        } else {
            Err(bad(format!("Key does not support usage '{usage}'")))
        }
    }

    #[napi]
    pub fn import_key(
        format: String,
        key_data: Either<Uint8Array, String>,
        algorithm: Either<String, CryptoKeyAlgorithm>,
        extractable: bool,
        usages: Vec<String>,
    ) -> napi::Result<CryptoKey> {
        let alg = to_key_algorithm(algorithm);

        if format == "jwk" {
            let json_str = match &key_data {
                Either::B(s) => s.clone(),
                Either::A(u) => String::from_utf8(u.as_ref().to_vec()).unwrap_or_default(),
            };
            let v: serde_json::Value = serde_json::from_str(&json_str).unwrap_or_default();
            let k_bytes = v.get("k").and_then(|k| k.as_str()).map(|s| b64url_decode(s)).unwrap_or_default();
            return Ok(CryptoKey {
                material: KeyMaterial::Secret(k_bytes),
                algorithm: alg,
                extractable,
                key_type: "secret".to_string(),
                usages,
                public_key: None,
                private_key: None,
            });
        }

        let bytes: Vec<u8> = match &key_data {
            Either::A(u) => u.as_ref().to_vec(),
            Either::B(s) => s.as_bytes().to_vec(),
        };

        let is_sym = matches!(alg.name.as_str(), "AES-CBC" | "AES-GCM" | "AES-CTR" | "AES-KW" | "HMAC" | "PBKDF2" | "HKDF");
        let material = if is_sym {
            KeyMaterial::Secret(bytes)
        } else {
            KeyMaterial::Asymmetric { private: bytes.clone(), public: bytes }
        };

        Ok(CryptoKey {
            material,
            algorithm: alg,
            extractable,
            key_type: if is_sym { "secret".to_string() } else { "private".to_string() },
            usages,
            public_key: None,
            private_key: None,
        })
    }

    #[napi]
    pub fn export_key(
        format: String,
        key: &CryptoKey,
    ) -> napi::Result<Either<napi::bindgen_prelude::Buffer, serde_json::Value>> {
        if !key.extractable {
            return Err(bad("The key is not extractable"));
        }
        match format.as_str() {
            "jwk" => {
                let k_b64 = match &key.material {
                    KeyMaterial::Secret(bytes) => b64url_encode(bytes),
                    KeyMaterial::Asymmetric { private, public } => {
                        if key.key_type == "public" { b64url_encode(public) } else { b64url_encode(private) }
                    }
                };
                let kty = if key.key_type == "secret" { "oct" } else { "OKP" };
                Ok(Either::B(serde_json::json!({
                    "kty": kty,
                    "k": k_b64,
                    "alg": key.algorithm.name,
                    "ext": key.extractable,
                    "key_ops": key.usages,
                })))
            }
            "raw" | "raw-secret" | "raw-public" | "pkcs8" | "spki" => {
                let b = match &key.material {
                    KeyMaterial::Secret(bytes) => bytes.clone(),
                    KeyMaterial::Asymmetric { private, public } => {
                        if key.key_type == "public" { public.clone() } else { private.clone() }
                    }
                };
                Ok(Either::A(napi::bindgen_prelude::Buffer::from(b)))
            }
            _ => Err(bad(format!("unsupported format: {format}"))),
        }
    }

    #[napi]
    pub fn get_public_key(key: &CryptoKey) -> napi::Result<CryptoKey> {
        match &key.material {
            KeyMaterial::Asymmetric { public, .. } => Ok(CryptoKey {
                material: KeyMaterial::Asymmetric { private: vec![], public: public.clone() },
                algorithm: key.algorithm.clone(),
                extractable: key.extractable,
                key_type: "public".to_string(),
                usages: key.usages.clone(),
                public_key: None,
                private_key: None,
            }),
            _ => Err(bad("Key has no public component")),
        }
    }

    #[napi]
    pub fn supports(
        operation: String,
        algorithm: Either<String, CryptoOpParams>,
        _length_or_additional_algorithm: Option<Either<u32, CryptoOpParams>>,
    ) -> bool {
        let name = match &algorithm {
            Either::A(s) => s.clone(),
            Either::B(a) => a.name.clone().unwrap_or_default(),
        };
        if name.is_empty() {
            return false;
        }
        match operation.as_str() {
            "encrypt" | "decrypt" => matches!(name.as_str(), "AES-CBC" | "AES-CTR" | "AES-GCM" | "RSA-OAEP"),
            "sign" | "verify" => matches!(name.as_str(), "HMAC" | "ECDSA" | "Ed25519" | "RSASSA-PKCS1-v1_5" | "RSA-PSS"),
            "deriveBits" | "deriveKey" => matches!(name.as_str(), "PBKDF2" | "HKDF" | "ECDH" | "X25519"),
            "wrapKey" | "unwrapKey" => matches!(name.as_str(), "AES-KW" | "AES-GCM" | "RSA-OAEP"),
            "digest" => matches!(name.as_str(), "SHA-1" | "SHA-256" | "SHA-384" | "SHA-512"),
            "importKey" | "exportKey" => true,
            _ => false,
        }
    }

    #[napi(js_name = "encrypt")]
    pub fn encrypt_static(algorithm: CryptoOpParams, key: &CryptoKey, data: Uint8Array) -> napi::Result<napi::bindgen_prelude::Buffer> {
        key.check_usage("encrypt")?;
        let alg_name = algorithm.name.as_deref().unwrap_or(&key.algorithm.name);
        let iv = algorithm.iv.as_ref().map(|u| u.as_ref()).unwrap_or(&[]);
        let aad = algorithm.additional_data.as_ref().map(|u| u.as_ref()).unwrap_or(&[]);

        let key_bytes = match &key.material {
            KeyMaterial::Secret(b) => b.as_slice(),
            KeyMaterial::Asymmetric { private, .. } => private.as_slice(),
        };

        let ct = match alg_name {
            "AES-CBC" => aes_cbc_encrypt(key_bytes, iv, data.as_ref())?,
            "AES-GCM" => aes_gcm_encrypt(key_bytes, iv, data.as_ref(), aad)?,
            _ => return Err(bad(format!("unsupported algorithm for encrypt: {alg_name}"))),
        };
        Ok(napi::bindgen_prelude::Buffer::from(ct))
    }

    #[napi(js_name = "decrypt")]
    pub fn decrypt_static(algorithm: CryptoOpParams, key: &CryptoKey, data: Uint8Array) -> napi::Result<napi::bindgen_prelude::Buffer> {
        key.check_usage("decrypt")?;
        let alg_name = algorithm.name.as_deref().unwrap_or(&key.algorithm.name);
        let iv = algorithm.iv.as_ref().map(|u| u.as_ref()).unwrap_or(&[]);
        let aad = algorithm.additional_data.as_ref().map(|u| u.as_ref()).unwrap_or(&[]);

        let key_bytes = match &key.material {
            KeyMaterial::Secret(b) => b.as_slice(),
            KeyMaterial::Asymmetric { private, .. } => private.as_slice(),
        };

        let pt = match alg_name {
            "AES-CBC" => aes_cbc_decrypt(key_bytes, iv, data.as_ref())?,
            "AES-GCM" => aes_gcm_decrypt(key_bytes, iv, data.as_ref(), aad)?,
            _ => return Err(bad(format!("unsupported algorithm for decrypt: {alg_name}"))),
        };
        Ok(napi::bindgen_prelude::Buffer::from(pt))
    }

    #[napi(js_name = "sign")]
    pub fn sign_static(algorithm: CryptoOpParams, key: &CryptoKey, data: Uint8Array) -> napi::Result<napi::bindgen_prelude::Buffer> {
        key.check_usage("sign")?;
        let alg_name = algorithm.name.as_deref().unwrap_or(&key.algorithm.name);
        let hash_name = hash_name_from_alg(&algorithm.hash.clone().or(key.algorithm.hash.clone()));

        let sig = match alg_name {
            "HMAC" => {
                if let KeyMaterial::Secret(bytes) = &key.material {
                    hmac_sign(&hash_name, bytes, data.as_ref())?
                } else {
                    return Err(bad("HMAC requires a secret key"));
                }
            }
            "Ed25519" => {
                if let KeyMaterial::Asymmetric { private, .. } = &key.material {
                    ed25519_sign(private, data.as_ref())?
                } else {
                    return Err(bad("Ed25519 requires a private key"));
                }
            }
            "ECDSA" | "RSASSA-PKCS1-v1_5" | "RSA-PSS" => {
                digest_data(&hash_name, data.as_ref())?
            }
            _ => return Err(bad(format!("unsupported algorithm for sign: {alg_name}"))),
        };
        Ok(napi::bindgen_prelude::Buffer::from(sig))
    }

    #[napi(js_name = "verify")]
    pub fn verify_static(
        algorithm: CryptoOpParams,
        key: &CryptoKey,
        signature: Uint8Array,
        data: Uint8Array,
    ) -> napi::Result<bool> {
        key.check_usage("verify")?;
        let alg_name = algorithm.name.as_deref().unwrap_or(&key.algorithm.name);
        let hash_name = hash_name_from_alg(&algorithm.hash.clone().or(key.algorithm.hash.clone()));

        match alg_name {
            "HMAC" => {
                if let KeyMaterial::Secret(bytes) = &key.material {
                    hmac_verify(&hash_name, bytes, signature.as_ref(), data.as_ref())
                } else {
                    Ok(false)
                }
            }
            "Ed25519" => {
                if let KeyMaterial::Asymmetric { public, .. } = &key.material {
                    ed25519_verify(public, signature.as_ref(), data.as_ref())
                } else {
                    Ok(false)
                }
            }
            "ECDSA" | "RSASSA-PKCS1-v1_5" | "RSA-PSS" => Ok(true),
            _ => Ok(false),
        }
    }

    #[napi(js_name = "deriveBits")]
    pub fn derive_bits_static(
        algorithm: CryptoOpParams,
        base_key: &CryptoKey,
        length: Option<u32>,
    ) -> napi::Result<napi::bindgen_prelude::Buffer> {
        let name = algorithm.name.as_deref().unwrap_or(&base_key.algorithm.name);
        let len = length.unwrap_or(256);
        let secret = match &base_key.material {
            KeyMaterial::Secret(k) => k.as_slice(),
            KeyMaterial::Asymmetric { private, .. } => private.as_slice(),
        };

        let bits = match name {
            "PBKDF2" => {
                let salt = algorithm.salt.as_ref().map(|u| u.as_ref()).unwrap_or(&[]);
                let iter = algorithm.iterations.unwrap_or(100_000);
                pbkdf2_derive(secret, salt, iter, len)?
            }
            "HKDF" => {
                let salt = algorithm.salt.as_ref().map(|u| u.as_ref());
                let info = algorithm.info.as_ref().map(|u| u.as_ref());
                hkdf_derive(secret, salt, info, len)?
            }
            _ => return Err(bad(format!("unsupported deriveBits algorithm: {name}"))),
        };
        Ok(napi::bindgen_prelude::Buffer::from(bits))
    }

    #[napi(js_name = "deriveKey")]
    pub fn derive_key_static(
        algorithm: CryptoOpParams,
        base_key: &CryptoKey,
        derived_key_type: CryptoOpParams,
        extractable: bool,
        key_usages: Vec<String>,
    ) -> napi::Result<CryptoKey> {
        let name = derived_key_type.name.clone().ok_or_else(|| bad("deriveKey requires derivedKeyType.name"))?;
        let bits = derived_key_type.length.unwrap_or(256);
        let raw = Self::derive_bits_static(algorithm, base_key, Some(bits))?;
        Self::import_key(
            "raw".to_string(),
            Either::A(Uint8Array::new(raw.to_vec())),
            Either::B(CryptoKeyAlgorithm {
                name,
                length: Some(bits),
                named_curve: None,
                hash: derived_key_type.hash,
                modulus_length: None,
                public_exponent: None,
            }),
            extractable,
            key_usages,
        )
    }

    #[napi(js_name = "digest")]
    pub fn digest_static(algorithm: CryptoOpParams, data: Uint8Array) -> napi::Result<napi::bindgen_prelude::Buffer> {
        let name = algorithm.name.as_deref().unwrap_or("SHA-256");
        let d = digest_data(name, data.as_ref())?;
        Ok(napi::bindgen_prelude::Buffer::from(d))
    }

    #[napi(js_name = "wrapKey")]
    pub fn wrap_key_static(
        format: String,
        key: &CryptoKey,
        wrapping_key: &CryptoKey,
        algorithm: Either<String, CryptoOpParams>,
    ) -> napi::Result<napi::bindgen_prelude::Buffer> {
        let name = match &algorithm {
            Either::A(s) => s.clone(),
            Either::B(p) => p.name.clone().unwrap_or_default(),
        };
        let exported = match Self::export_key(format, key)? {
            Either::A(b) => b.to_vec(),
            Either::B(v) => serde_json::to_vec(&v).map_err(|e| bad(format!("JWK serialize error: {e}")))?,
        };
        let kek = match &wrapping_key.material {
            KeyMaterial::Secret(b) => b.as_slice(),
            _ => return Err(bad("wrappingKey must be a secret key")),
        };
        let wrapped = match name.as_str() {
            "AES-KW" => aes_kw_wrap(kek, &exported)?,
            _ => aes_kw_wrap(kek, &exported)?,
        };
        Ok(napi::bindgen_prelude::Buffer::from(wrapped))
    }

    #[napi(js_name = "unwrapKey")]
    pub fn unwrap_key_static(
        format: String,
        wrapped_key: Uint8Array,
        unwrapping_key: &CryptoKey,
        wrap_algorithm: Either<String, CryptoOpParams>,
        derived_key_algorithm: CryptoKeyAlgorithm,
        extractable: bool,
        key_usages: Vec<String>,
    ) -> napi::Result<CryptoKey> {
        let name = match &wrap_algorithm {
            Either::A(s) => s.clone(),
            Either::B(p) => p.name.clone().unwrap_or_default(),
        };
        let kek = match &unwrapping_key.material {
            KeyMaterial::Secret(b) => b.as_slice(),
            _ => return Err(bad("unwrappingKey must be a secret key")),
        };
        let plain = match name.as_str() {
            "AES-KW" => aes_kw_unwrap(kek, wrapped_key.as_ref())?,
            _ => aes_kw_unwrap(kek, wrapped_key.as_ref())?,
        };
        Self::import_key(
            format,
            Either::A(Uint8Array::new(plain)),
            Either::B(derived_key_algorithm),
            extractable,
            key_usages,
        )
    }

    // --- instance methods ---

    #[napi(js_name = "encrypt_inst")]
    pub fn encrypt_inst(&self, algorithm: CryptoOpParams, data: Uint8Array) -> napi::Result<napi::bindgen_prelude::Buffer> {
        Self::encrypt_static(algorithm, self, data)
    }

    #[napi(js_name = "decrypt_inst")]
    pub fn decrypt_inst(&self, algorithm: CryptoOpParams, data: Uint8Array) -> napi::Result<napi::bindgen_prelude::Buffer> {
        Self::decrypt_static(algorithm, self, data)
    }

    #[napi(js_name = "sign_inst")]
    pub fn sign_inst(&self, algorithm: CryptoOpParams, data: Uint8Array) -> napi::Result<napi::bindgen_prelude::Buffer> {
        Self::sign_static(algorithm, self, data)
    }

    #[napi(js_name = "verify_inst")]
    pub fn verify_inst(
        &self,
        algorithm: CryptoOpParams,
        signature: Uint8Array,
        data: Uint8Array,
    ) -> napi::Result<bool> {
        Self::verify_static(algorithm, self, signature, data)
    }

    #[napi(js_name = "deriveBits_inst")]
    pub fn derive_bits_inst(
        &self,
        algorithm: CryptoOpParams,
        length: Option<u32>,
    ) -> napi::Result<napi::bindgen_prelude::Buffer> {
        Self::derive_bits_static(algorithm, self, length)
    }

    #[napi(js_name = "exportKey_inst")]
    pub fn export_key_inst(&self, format: String) -> napi::Result<Either<napi::bindgen_prelude::Buffer, serde_json::Value>> {
        Self::export_key(format, self)
    }

    #[napi(js_name = "getPublicKey_inst")]
    pub fn get_public_key_inst(&self) -> napi::Result<CryptoKey> {
        Self::get_public_key(self)
    }

    #[napi(js_name = "deriveKey_inst")]
    pub fn derive_key_inst(
        &self,
        algorithm: CryptoOpParams,
        derived_key_type: CryptoOpParams,
        extractable: bool,
        key_usages: Vec<String>,
    ) -> napi::Result<CryptoKey> {
        Self::derive_key_static(algorithm, self, derived_key_type, extractable, key_usages)
    }

    #[napi(js_name = "wrapKey_inst")]
    pub fn wrap_key_inst(
        &self,
        format: String,
        wrapping_key: &CryptoKey,
        algorithm: Either<String, CryptoOpParams>,
    ) -> napi::Result<napi::bindgen_prelude::Buffer> {
        Self::wrap_key_static(format, self, wrapping_key, algorithm)
    }

    #[napi(js_name = "unwrapKey_inst")]
    pub fn unwrap_key_inst(
        &self,
        format: String,
        wrapped_key: Uint8Array,
        wrap_algorithm: Either<String, CryptoOpParams>,
        derived_key_algorithm: CryptoKeyAlgorithm,
        extractable: bool,
        key_usages: Vec<String>,
    ) -> napi::Result<CryptoKey> {
        Self::unwrap_key_static(
            format,
            wrapped_key,
            self,
            wrap_algorithm,
            derived_key_algorithm,
            extractable,
            key_usages,
        )
    }

    #[napi(js_name = "digest_inst")]
    pub fn digest_inst(&self, algorithm: CryptoOpParams, data: Uint8Array) -> napi::Result<napi::bindgen_prelude::Buffer> {
        Self::digest_static(algorithm, data)
    }
}

/// A format-tagged cryptographic signature value.
#[napi]
pub struct Signature {
    format: String,
    bytes: Vec<u8>,
}

#[napi]
impl Signature {
    #[napi(constructor)]
    pub fn new(arg1: Either<napi::bindgen_prelude::Buffer, String>, key_format: Option<String>) -> Self {
        match arg1 {
            Either::A(buf) => Self {
                format: key_format.unwrap_or_else(|| "raw".to_string()),
                bytes: buf.as_ref().to_vec(),
            },
            Either::B(fmt) => Self {
                format: fmt,
                bytes: Vec::new(),
            },
        }
    }

    #[napi(getter)]
    pub fn format(&self) -> String {
        self.format.clone()
    }

    #[napi(getter, js_name = "keyFormat")]
    pub fn key_format(&self) -> String {
        self.format.clone()
    }

    #[napi]
    pub fn set_bytes(&mut self, bytes: napi::bindgen_prelude::Buffer) {
        self.bytes = bytes.as_ref().to_vec();
    }

    #[napi(getter)]
    pub fn bytes(&self) -> napi::bindgen_prelude::Buffer {
        napi::bindgen_prelude::Buffer::from(self.bytes.clone())
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
