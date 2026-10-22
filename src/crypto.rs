//! Transparent file encryption ("crypt") layered on top of the gix repository.
//!
//! This module ports a standalone crypto implementation into a single file and
//! exposes it through a [`Crypto`] NAPI handle obtained from
//! [`crate::repository::Repository::crypt`].

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aes::cipher::{BlockDecrypt, BlockEncrypt};
use aes::{Aes128, Aes256};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Aes256Gcm, Nonce as GcmNonce};
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use chacha20poly1305::aead::{Aead as _, KeyInit as _};
use chacha20poly1305::{ChaCha20Poly1305, Key as ChaChaKey, Nonce as ChaChaNonce};
use ed25519_dalek::{Signer as _, Verifier as _};
use hmac::{Hmac, Mac};
use multi_sig::prelude::*;
use multi_util::CodecInfo;
use napi::bindgen_prelude::{Buffer, ClassInstance, Either, Env, JsValue as _, Uint8Array, Unknown};
use napi::Result;
use napi_derive::napi;
use rand::RngCore;
use rsa::signature::SignatureEncoding as _;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha384, Sha512};
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
        rand::thread_rng().fill_bytes(&mut key);
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
        rand::thread_rng().fill_bytes(&mut nonce_bytes);

        let cipher = ChaCha20Poly1305::new(ChaChaKey::from_slice(&self.key));
        let ciphertext = cipher
            .encrypt(ChaChaNonce::from_slice(&nonce_bytes), plaintext)
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

        let cipher = ChaCha20Poly1305::new(ChaChaKey::from_slice(&self.key));
        cipher
            .decrypt(ChaChaNonce::from_slice(nonce_bytes), enc)
            .map_err(|_| CryptoError::Crypto("decryption failed (wrong key or tampered)".into()))
    }

    pub fn is_encrypted(data: &[u8]) -> bool {
        data.len() >= MAGIC_HEADER.len() && &data[..MAGIC_HEADER.len()] == MAGIC_HEADER
    }
}

// ---------------------------------------------------------------------------
// KeyManager (on-disk key storage under <git-dir>/crypt)
// ---------------------------------------------------------------------------

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
    pub fn from_codec(codec: Either<String, u32>) -> Result<Self> {
        let c = match codec {
            Either::A(name) => Codec::try_from(name.as_str())
                .map_err(|e| napi::Error::from_reason(format!("unknown codec '{name}': {e}")))?,
            Either::B(code) => Codec::try_from(code)
                .map_err(|e| napi::Error::from_reason(format!("unknown codec {code}: {e}")))?,
        };
        Ok(Self {
            code: c.code() as u32,
            name: c.as_str().to_string(),
        })
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
    inner: multi_sig::Multisig,
    key_format: Option<String>,
}

#[napi]
impl MultiSignature {
    #[napi(constructor)]
    pub fn new(bytes: Buffer, key_format: Option<String>) -> Result<Self> {
        multi_sig::Multisig::try_from(bytes.as_ref())
            .map(|inner| Self { inner, key_format })
            .map_err(|e| napi::Error::from_reason(format!("decode multisig: {e}")))
    }

    #[napi(getter, js_name = "keyFormat")]
    pub fn key_format(&self) -> Option<String> {
        self.key_format.clone()
    }

    #[napi]
    pub fn from_bytes(&self) -> Buffer {
        Buffer::from(Vec::from(self.inner.clone()))
    }

    #[napi]
    pub fn from_hex(&self) -> String {
        let encoded: BaseEncoded<multi_sig::Multisig> =
            BaseEncoded::from(self.inner.clone());
        encoded.to_string()
    }

    #[napi(getter)]
    pub fn codec(&self) -> String {
        CodecInfo::codec(&self.inner).as_str().to_string()
    }

    #[napi(getter)]
    pub fn code(&self) -> u32 {
        CodecInfo::codec(&self.inner).code() as u32
    }

    #[napi]
    pub fn signature_bytes(&self) -> Result<SignatureBytes> {
        let data = multi_sig::ViewBuilder::new(&self.inner)
            .data()
            .build()
            .map_err(|e| napi::Error::from_reason(format!("data view: {e}")))?;
        let bytes = data
            .sig_bytes()
            .map_err(|e| napi::Error::from_reason(format!("sig bytes: {e}")))?;
        Ok(SignatureBytes::new(Buffer::from(bytes)))
    }

    #[napi(getter)]
    pub fn bytes(&self) -> Buffer {
        Buffer::from(Vec::from(self.inner.clone()))
    }

    #[napi(getter)]
    pub fn message(&self) -> Buffer {
        Buffer::from(self.inner.message.clone())
    }

    #[napi(getter)]
    pub fn is_combined(&self) -> bool {
        !self.inner.message.is_empty()
    }

    #[napi(getter)]
    pub fn sig_index(&self) -> Option<u32> {
        self.inner.sig_index()
    }

    #[napi(getter)]
    pub fn depth(&self) -> Option<u8> {
        self.inner.depth()
    }
}

fn resolve_codec(codec: Either<String, u32>) -> Result<Codec> {
    match codec {
        Either::A(name) => Codec::try_from(name.as_str())
            .map_err(|e| napi::Error::from_reason(format!("unknown codec '{name}': {e}"))),
        Either::B(code) => Codec::try_from(code)
            .map_err(|e| napi::Error::from_reason(format!("unknown codec {code}: {e}"))),
    }
}

#[napi]
pub struct SignatureBuilder {
    inner: multi_sig::Builder,
}

#[napi]
impl SignatureBuilder {
    #[napi(constructor)]
    pub fn new(codec: Either<String, u32>) -> Result<Self> {
        let c = resolve_codec(codec)?;
        Ok(Self {
            inner: multi_sig::Builder::new(c),
        })
    }

    #[napi]
    pub fn with_message(&mut self, message: Buffer) -> &Self {
        self.inner = std::mem::take(&mut self.inner).with_message_bytes(&message);
        self
    }

    #[napi]
    pub fn with_signature(&mut self, signature: Buffer) -> &Self {
        self.inner = std::mem::take(&mut self.inner).with_signature_bytes(&signature);
        self
    }

    #[napi]
    pub fn with_payload_encoding(&mut self, codec: Either<String, u32>) -> Result<&Self> {
        let c = resolve_codec(codec)?;
        self.inner = std::mem::take(&mut self.inner).with_payload_encoding(c);
        Ok(self)
    }

    #[napi]
    pub fn with_scheme(&mut self, scheme: u32) -> &Self {
        self.inner = std::mem::take(&mut self.inner).with_scheme(scheme as u8);
        self
    }

    #[napi]
    pub fn with_threshold(&mut self, threshold: u32) -> &Self {
        self.inner = std::mem::take(&mut self.inner).with_threshold(threshold as usize);
        self
    }

    #[napi]
    pub fn with_limit(&mut self, limit: u32) -> &Self {
        self.inner = std::mem::take(&mut self.inner).with_limit(limit as usize);
        self
    }

    #[napi]
    pub fn with_identifier(&mut self, identifier: Buffer) -> &Self {
        self.inner = std::mem::take(&mut self.inner).with_identifier(&identifier);
        self
    }

    #[napi]
    pub fn with_threshold_data(&mut self, data: Buffer) -> &Self {
        self.inner = std::mem::take(&mut self.inner).with_threshold_data(&data);
        self
    }

    #[napi]
    pub fn with_sig_index(&mut self, index: u32) -> &Self {
        self.inner = std::mem::take(&mut self.inner).with_sig_index(index);
        self
    }

    #[napi]
    pub fn with_depth(&mut self, depth: u32) -> &Self {
        self.inner = std::mem::take(&mut self.inner).with_depth(depth as u8);
        self
    }

    #[napi]
    pub fn add_signature_share(&mut self, share: &MultiSignature) -> &Self {
        self.inner = std::mem::take(&mut self.inner).add_signature_share(&share.inner);
        self
    }

    #[napi]
    pub fn with_disclosure(
        &mut self,
        mode: u32,
        meta_key: Option<Buffer>,
        threshold: u32,
        limit: u32,
    ) -> Result<&Self> {
        let disclosure = match mode {
            0 => multi_sig::ThresholdDisclosure::Full,
            1 => multi_sig::ThresholdDisclosure::Partial,
            2 => multi_sig::ThresholdDisclosure::FullConfidential,
            other => {
                return Err(napi::Error::from_reason(format!(
                    "invalid disclosure mode {other}; expected 0 (Full), 1 (Partial) or 2 (FullConfidential)"
                )))
            }
        };
        let meta: Option<&[u8]> = meta_key.as_ref().map(|b| b.as_ref());
        self.inner = std::mem::take(&mut self.inner).with_disclosure(
            disclosure,
            meta,
            threshold as usize,
            limit as usize,
        );
        Ok(self)
    }

    #[napi(factory)]
    pub fn new_from_bls_signature(codec: Either<String, u32>, signature: Buffer) -> Result<Self> {
        let c = resolve_codec(codec)?;
        let inner = match c {
            Codec::Bls12381G1Msig => {
                let sig = blsful::Signature::<blsful::Bls12381G1Impl>::try_from(signature.as_ref())
                    .map_err(|e| {
                        napi::Error::from_reason(format!("invalid BLS12-381 G1 signature: {e}"))
                    })?;
                multi_sig::Builder::new_from_bls_signature_with_codec(c, &sig)
                    .map_err(|e| napi::Error::from_reason(format!("bls signature: {e}")))?
            }
            Codec::Bls12381G2Msig => {
                let sig = blsful::Signature::<blsful::Bls12381G2Impl>::try_from(signature.as_ref())
                    .map_err(|e| {
                        napi::Error::from_reason(format!("invalid BLS12-381 G2 signature: {e}"))
                    })?;
                multi_sig::Builder::new_from_bls_signature_with_codec(c, &sig)
                    .map_err(|e| napi::Error::from_reason(format!("bls signature: {e}")))?
            }
            other => {
                return Err(napi::Error::from_reason(format!(
                    "{other:?} is not a BLS12-381 signature codec"
                )))
            }
        };
        Ok(Self { inner })
    }

    #[napi(factory)]
    pub fn new_from_bls_signature_share(
        codec: Either<String, u32>,
        threshold: u32,
        limit: u32,
        share: Buffer,
    ) -> Result<Self> {
        let c = resolve_codec(codec)?;
        let inner = match c {
            Codec::Bls12381G1ShareMsig => {
                let s = blsful::SignatureShare::<blsful::Bls12381G1Impl>::try_from(share.as_ref())
                    .map_err(|e| {
                        napi::Error::from_reason(format!("invalid BLS12-381 G1 share: {e}"))
                    })?;
                multi_sig::Builder::new_from_bls_signature_share_with_codec(
                    c,
                    threshold as usize,
                    limit as usize,
                    &s,
                )
                .map_err(|e| napi::Error::from_reason(format!("bls share: {e}")))?
            }
            Codec::Bls12381G2ShareMsig => {
                let s = blsful::SignatureShare::<blsful::Bls12381G2Impl>::try_from(share.as_ref())
                    .map_err(|e| {
                        napi::Error::from_reason(format!("invalid BLS12-381 G2 share: {e}"))
                    })?;
                multi_sig::Builder::new_from_bls_signature_share_with_codec(
                    c,
                    threshold as usize,
                    limit as usize,
                    &s,
                )
                .map_err(|e| napi::Error::from_reason(format!("bls share: {e}")))?
            }
            other => {
                return Err(napi::Error::from_reason(format!(
                    "{other:?} is not a BLS12-381 signature share codec"
                )))
            }
        };
        Ok(Self { inner })
    }

    #[napi]
    pub fn generate_meta_key() -> Buffer {
        Buffer::from(multi_sig::generate_meta_key().to_vec())
    }

    #[napi]
    pub fn try_build(&self) -> Result<MultiSignature> {
        self.inner
            .clone()
            .try_build()
            .map(|inner| MultiSignature { inner, key_format: None })
            .map_err(|e| napi::Error::from_reason(format!("build multisig: {e}")))
    }

    #[napi]
    pub fn try_build_encoded(&self) -> Result<String> {
        self.inner
            .clone()
            .try_build_encoded()
            .map(|encoded| encoded.to_string())
            .map_err(|e| napi::Error::from_reason(format!("build multisig: {e}")))
    }
}

/// A crypt handle bound to an open repository. Obtain via `Repository::crypt()`.
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
// WebCrypto CryptoKey Implementation
// ---------------------------------------------------------------------------

fn bad(msg: impl Into<String>) -> napi::Error {
    napi::Error::new(napi::Status::InvalidArg, msg.into())
}

/// Base algorithm object. Always present; carries at least `name`.
#[napi(object)]
#[derive(Clone)]
pub struct KeyAlgorithm {
    pub name: String,
}

/// The normalized `algorithm` dictionary of a [`CryptoKey`].
#[napi(object)]
#[derive(Clone)]
pub struct CryptoKeyAlgorithm {
    pub name: String,
    pub length: Option<u32>,
    pub named_curve: Option<String>,
    pub hash: Option<KeyAlgorithm>,
    pub modulus_length: Option<u32>,
    pub public_exponent: Option<Vec<u8>>,
}

/// Parameters of a crypto operation.
#[napi(object)]
pub struct CryptoOpParams {
    pub name: Option<String>,
    pub iv: Option<Uint8Array>,
    pub additional_data: Option<Uint8Array>,
    pub tag_length: Option<u32>,
    pub iterations: Option<u32>,
    pub salt: Option<Uint8Array>,
    pub info: Option<Uint8Array>,
    pub label: Option<Uint8Array>,
    pub hash: Option<KeyAlgorithm>,
    pub salt_length: Option<u32>,
    pub public: Option<ClassInstance<'static, CryptoKey>>,
    pub length: Option<u32>,
}

/// Key material backing a [`CryptoKey`].
#[derive(Clone)]
enum KeyMaterial {
    Secret(Vec<u8>),
    Ed25519Private(ed25519_dalek::SigningKey),
    Ed25519Public(ed25519_dalek::VerifyingKey),
    X25519Private(x25519_dalek::StaticSecret, x25519_dalek::PublicKey),
    X25519Public(x25519_dalek::PublicKey),
    EcPrivate(p256::SecretKey),
    EcPublic(p256::PublicKey),
    RsaPrivate(rsa::RsaPrivateKey),
    RsaPublic(rsa::RsaPublicKey),
}

/// A Web Crypto `CryptoKey`.
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AlgKind {
    Aes,
    ChaCha,
    Mac,
    Ecdsa,
    Ecdh,
    Ed25519,
    X25519,
    RsaOaep,
    RsaPss,
    RsaPkcs1,
    Kdf,
}

fn alg_kind(name: &str) -> Option<AlgKind> {
    Some(match name {
        "AES-CBC" | "AES-CTR" | "AES-GCM" | "AES-KW" | "AES-OCB" => AlgKind::Aes,
        "ChaCha20-Poly1305" => AlgKind::ChaCha,
        "HMAC" | "KMAC128" | "KMAC256" => AlgKind::Mac,
        "ECDSA" => AlgKind::Ecdsa,
        "ECDH" => AlgKind::Ecdh,
        "Ed25519" | "Ed448" => AlgKind::Ed25519,
        "X25519" | "X448" => AlgKind::X25519,
        "RSA-OAEP" => AlgKind::RsaOaep,
        "RSA-PSS" => AlgKind::RsaPss,
        "RSASSA-PKCS1-v1_5" => AlgKind::RsaPkcs1,
        "PBKDF2" | "HKDF" | "Argon2d" | "Argon2i" | "Argon2id" => AlgKind::Kdf,
        _ => return None,
    })
}

fn is_symmetric(kind: AlgKind) -> bool {
    matches!(kind, AlgKind::Aes | AlgKind::ChaCha | AlgKind::Mac)
}

fn hash_name(hash: &Option<KeyAlgorithm>) -> String {
    hash.as_ref()
        .map(|h| h.name.clone())
        .unwrap_or_else(|| "SHA-256".to_string())
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

type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;
type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

fn aes_cbc_encrypt(key: &[u8], iv: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    if key.len() == 16 {
        let cipher = Aes128CbcEnc::new_from_slices(key, iv)
            .map_err(|e| bad(format!("invalid AES-CBC key/iv: {e}")))?;
        let mut buf = vec![0u8; plaintext.len() + 16];
        buf[..plaintext.len()].copy_from_slice(plaintext);
        let ct = cipher.encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
            .map_err(|_| bad("AES-CBC encryption failed"))?;
        Ok(ct.to_vec())
    } else if key.len() == 32 {
        let cipher = Aes256CbcEnc::new_from_slices(key, iv)
            .map_err(|e| bad(format!("invalid AES-CBC key/iv: {e}")))?;
        let mut buf = vec![0u8; plaintext.len() + 16];
        buf[..plaintext.len()].copy_from_slice(plaintext);
        let ct = cipher.encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
            .map_err(|_| bad("AES-CBC encryption failed"))?;
        Ok(ct.to_vec())
    } else {
        Err(bad("AES-CBC key length must be 128 or 256 bits"))
    }
}

fn aes_cbc_decrypt(key: &[u8], iv: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
    if key.len() == 16 {
        let cipher = Aes128CbcDec::new_from_slices(key, iv)
            .map_err(|e| bad(format!("invalid AES-CBC key/iv: {e}")))?;
        let mut buf = ciphertext.to_vec();
        let pt = cipher.decrypt_padded_mut::<Pkcs7>(&mut buf)
            .map_err(|_| bad("AES-CBC decryption failed"))?;
        Ok(pt.to_vec())
    } else if key.len() == 32 {
        let cipher = Aes256CbcDec::new_from_slices(key, iv)
            .map_err(|e| bad(format!("invalid AES-CBC key/iv: {e}")))?;
        let mut buf = ciphertext.to_vec();
        let pt = cipher.decrypt_padded_mut::<Pkcs7>(&mut buf)
            .map_err(|_| bad("AES-CBC decryption failed"))?;
        Ok(pt.to_vec())
    } else {
        Err(bad("AES-CBC key length must be 128 or 256 bits"))
    }
}

fn aes_kw_wrap(kek: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    if plaintext.len() % 8 != 0 || plaintext.len() < 16 {
        return Err(bad(
            "AES-KW key material must be a multiple of 8 bytes and at least 16 bytes",
        ));
    }

    let n = plaintext.len() / 8;
    let mut blocks: Vec<[u8; 8]> = plaintext
        .chunks_exact(8)
        .map(|c| c.try_into().unwrap())
        .collect();
    let mut a = [0xA6u8; 8];

    for i in 1..=6u64 {
        for (j, blk) in blocks.iter_mut().enumerate() {
            let mut block = [0u8; 16];
            block[..8].copy_from_slice(&a);
            block[8..].copy_from_slice(blk);

            if kek.len() == 16 {
                let cipher = Aes128::new_from_slice(kek).map_err(|e| bad(e.to_string()))?;
                let mut generic_block = aes::cipher::generic_array::GenericArray::from(block);
                cipher.encrypt_block(&mut generic_block);
                block = generic_block.into();
            } else if kek.len() == 32 {
                let cipher = Aes256::new_from_slice(kek).map_err(|e| bad(e.to_string()))?;
                let mut generic_block = aes::cipher::generic_array::GenericArray::from(block);
                cipher.encrypt_block(&mut generic_block);
                block = generic_block.into();
            } else {
                return Err(bad("AES-KW key length must be 128 or 256 bits"));
            }

            a.copy_from_slice(&block[..8]);
            blk.copy_from_slice(&block[8..]);

            let t = (i * n as u64 + j as u64).to_be_bytes();
            for k in 0..8 {
                a[k] ^= t[k];
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

fn aes_kw_unwrap(kek: &[u8], wrapped: &[u8]) -> Result<Vec<u8>> {
    if wrapped.len() % 8 != 0 || wrapped.len() < 24 {
        return Err(bad(
            "AES-KW wrapped key must be a multiple of 8 bytes and at least 24 bytes",
        ));
    }

    let n = wrapped.len() / 8 - 1;
    let mut a = [0u8; 8];
    a.copy_from_slice(&wrapped[..8]);
    let mut blocks: Vec<[u8; 8]> = wrapped[8..]
        .chunks_exact(8)
        .map(|c| c.try_into().unwrap())
        .collect();

    for i in (1..=6u64).rev() {
        for j in (0..n).rev() {
            let t = (i * n as u64 + j as u64).to_be_bytes();
            for k in 0..8 {
                a[k] ^= t[k];
            }

            let mut block = [0u8; 16];
            block[..8].copy_from_slice(&a);
            block[8..].copy_from_slice(&blocks[j]);

            if kek.len() == 16 {
                let cipher = Aes128::new_from_slice(kek).map_err(|e| bad(e.to_string()))?;
                let mut generic_block = aes::cipher::generic_array::GenericArray::from(block);
                cipher.decrypt_block(&mut generic_block);
                block = generic_block.into();
            } else if kek.len() == 32 {
                let cipher = Aes256::new_from_slice(kek).map_err(|e| bad(e.to_string()))?;
                let mut generic_block = aes::cipher::generic_array::GenericArray::from(block);
                cipher.decrypt_block(&mut generic_block);
                block = generic_block.into();
            } else {
                return Err(bad("AES-KW key length must be 128 or 256 bits"));
            }

            a.copy_from_slice(&block[..8]);
            blocks[j].copy_from_slice(&block[8..]);
        }
    }

    if a != [0xA6u8; 8] {
        return Err(bad("AES-KW integrity check failed"));
    }

    let mut out = Vec::with_capacity(n * 8);
    for b in &blocks {
        out.extend_from_slice(b);
    }
    Ok(out)
}

#[napi]
impl CryptoKey {
    #[napi(constructor)]
    pub fn new(
        algorithm: Either<String, CryptoKeyAlgorithm>,
        extractable: bool,
        usages: Vec<String>,
    ) -> Result<Self> {
        Self::build_key(algorithm, extractable, usages)
    }

    fn build_key(
        algorithm: Either<String, CryptoKeyAlgorithm>,
        extractable: bool,
        usages: Vec<String>,
    ) -> Result<Self> {
        let mut algorithm = to_key_algorithm(algorithm);
        let kind = alg_kind(&algorithm.name)
            .ok_or_else(|| bad(format!("unsupported algorithm: {}", algorithm.name)))?;

        let material = match kind {
            AlgKind::Aes | AlgKind::ChaCha => {
                let bits = algorithm.length.unwrap_or(256);
                let mut b = vec![0u8; (bits / 8) as usize];
                rand::thread_rng().fill_bytes(&mut b);
                KeyMaterial::Secret(b)
            }
            AlgKind::Mac => {
                let len = match algorithm.length {
                    Some(l) => (l / 8) as usize,
                    None => 32,
                };
                let mut b = vec![0u8; len];
                rand::thread_rng().fill_bytes(&mut b);
                KeyMaterial::Secret(b)
            }
            AlgKind::Ecdsa | AlgKind::Ecdh => {
                let secret = p256::SecretKey::random(&mut rand::thread_rng());
                algorithm.named_curve = Some("P-256".to_string());
                KeyMaterial::EcPrivate(secret)
            }
            AlgKind::Ed25519 => {
                let mut bytes = [0u8; 32];
                rand::thread_rng().fill_bytes(&mut bytes);
                let secret = ed25519_dalek::SigningKey::from_bytes(&bytes);
                KeyMaterial::Ed25519Private(secret)
            }
            AlgKind::X25519 => {
                let secret = x25519_dalek::StaticSecret::random_from_rng(&mut rand::thread_rng());
                let public = x25519_dalek::PublicKey::from(&secret);
                KeyMaterial::X25519Private(secret, public)
            }
            AlgKind::RsaOaep | AlgKind::RsaPss | AlgKind::RsaPkcs1 => {
                let bits = algorithm.modulus_length.unwrap_or(2048) as usize;
                let priv_key = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), bits)
                    .map_err(|e| bad(format!("RSA keygen error: {e}")))?;
                algorithm.modulus_length = Some(bits as u32);
                algorithm.public_exponent = Some(vec![1, 0, 1]);
                if algorithm.hash.is_none() {
                    algorithm.hash = Some(KeyAlgorithm {
                        name: "SHA-256".to_string(),
                    });
                }
                KeyMaterial::RsaPrivate(priv_key)
            }
            AlgKind::Kdf => {
                return Err(bad(format!(
                    "{} does not support key generation; use importKey",
                    algorithm.name
                )))
            }
        };

        if is_symmetric(kind) {
            if let KeyMaterial::Secret(ref b) = material {
                algorithm.length = Some((b.len() * 8) as u32);
            }
            return Ok(Self {
                material,
                algorithm,
                extractable,
                key_type: "secret".to_string(),
                usages,
                public_key: None,
                private_key: None,
            });
        }

        let pub_material = match &material {
            KeyMaterial::Ed25519Private(sk) => KeyMaterial::Ed25519Public(sk.verifying_key()),
            KeyMaterial::X25519Private(_, pk) => KeyMaterial::X25519Public(*pk),
            KeyMaterial::EcPrivate(sk) => KeyMaterial::EcPublic(sk.public_key()),
            KeyMaterial::RsaPrivate(sk) => KeyMaterial::RsaPublic(sk.to_public_key()),
            _ => unreachable!(),
        };

        let pub_key = Self {
            material: pub_material,
            algorithm: algorithm.clone(),
            extractable,
            key_type: "public".to_string(),
            usages: usages.clone(),
            public_key: None,
            private_key: None,
        };
        let priv_key = Self {
            material: material.clone(),
            algorithm: algorithm.clone(),
            extractable,
            key_type: "private".to_string(),
            usages: usages.clone(),
            public_key: None,
            private_key: None,
        };

        Ok(Self {
            material,
            algorithm,
            extractable,
            key_type: "private".to_string(),
            usages,
            public_key: Some(Box::new(pub_key)),
            private_key: Some(Box::new(priv_key)),
        })
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

    #[napi(getter, js_name = "publicKey")]
    pub fn get_public_key_prop(&self) -> Option<CryptoKey> {
        self.public_key.as_deref().cloned()
    }

    #[napi(getter, js_name = "privateKey")]
    pub fn get_private_key_prop(&self) -> Option<CryptoKey> {
        self.private_key.as_deref().cloned()
    }

    #[napi]
    pub fn import_key(
        env: Env,
        format: String,
        key_data: Either<Uint8Array, Unknown>,
        algorithm: Either<String, CryptoKeyAlgorithm>,
        extractable: bool,
        usages: Vec<String>,
    ) -> Result<CryptoKey> {
        let mut alg = to_key_algorithm(algorithm);
        let kind = alg_kind(&alg.name)
            .ok_or_else(|| bad(format!("unsupported algorithm: {}", alg.name)))?;

        let (material, key_type): (KeyMaterial, &str) = match format.as_str() {
            "raw" | "raw-secret" => {
                let bytes = match &key_data {
                    Either::A(b) => b.as_ref().to_vec(),
                    Either::B(_) => return Err(bad("raw key material must be bytes")),
                };
                match kind {
                    AlgKind::Aes | AlgKind::ChaCha | AlgKind::Mac | AlgKind::Kdf => {
                        alg.length = Some((bytes.len() * 8) as u32);
                        (KeyMaterial::Secret(bytes), "secret")
                    }
                    AlgKind::Ed25519 => {
                        if bytes.len() == 32 {
                            let sk = ed25519_dalek::SigningKey::from_bytes(
                                bytes.as_slice().try_into().unwrap(),
                            );
                            (KeyMaterial::Ed25519Private(sk), "private")
                        } else {
                            return Err(bad("Ed25519 raw key must be 32 bytes"));
                        }
                    }
                    AlgKind::X25519 => {
                        if bytes.len() == 32 {
                            let mut arr = [0u8; 32];
                            arr.copy_from_slice(&bytes);
                            let secret = x25519_dalek::StaticSecret::from(arr);
                            let public = x25519_dalek::PublicKey::from(&secret);
                            (KeyMaterial::X25519Private(secret, public), "private")
                        } else {
                            return Err(bad("X25519 raw key must be 32 bytes"));
                        }
                    }
                    _ => return Err(bad(format!("{} cannot be imported as raw", alg.name))),
                }
            }
            "jwk" => {
                let json_val: serde_json::Value = match &key_data {
                    Either::B(u) => env.from_js_value(*u)?,
                    Either::A(_) => return Err(bad("JWK must be an object or JSON string")),
                };

                let kty = json_val
                    .get("kty")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| bad("JWK missing kty"))?;

                match kty {
                    "oct" => {
                        let k_b64 = json_val
                            .get("k")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| bad("JWK oct missing k"))?;
                        let bytes = base64_url_decode(k_b64)?;
                        alg.length = Some((bytes.len() * 8) as u32);
                        (KeyMaterial::Secret(bytes), "secret")
                    }
                    _ => return Err(bad(format!("unsupported JWK kty: {kty}"))),
                }
            }
            other => return Err(bad(format!("unsupported key format: {other}"))),
        };

        Ok(CryptoKey {
            material,
            algorithm: alg,
            extractable,
            key_type: key_type.to_string(),
            usages,
            public_key: None,
            private_key: None,
        })
    }

    #[napi(js_name = "import")]
    pub fn import_static(
        env: Env,
        format: String,
        key_data: Either<Uint8Array, Unknown>,
        algorithm: Either<String, CryptoKeyAlgorithm>,
        extractable: bool,
        usages: Vec<String>,
    ) -> Result<CryptoKey> {
        Self::import_key(env, format, key_data, algorithm, extractable, usages)
    }

    #[napi]
    pub fn export_key(
        format: String,
        key: &CryptoKey,
    ) -> Result<Either<Buffer, serde_json::Value>> {
        if !key.extractable {
            return Err(bad("The key is not extractable"));
        }
        match format.as_str() {
            "jwk" => match &key.material {
                KeyMaterial::Secret(bytes) => {
                    let k_b64 = base64_url_encode(bytes);
                    let alg_str = match key.algorithm.name.as_str() {
                        "HMAC" => format!("HS{}", key.algorithm.length.unwrap_or(256)),
                        other => other.to_string(),
                    };
                    let jwk = serde_json::json!({
                        "kty": "oct",
                        "k": k_b64,
                        "alg": alg_str,
                        "ext": key.extractable,
                        "key_ops": key.usages,
                    });
                    Ok(Either::B(jwk))
                }
                _ => Err(bad("export JWK is supported for symmetric secret keys")),
            },
            "raw" | "raw-secret" => match &key.material {
                KeyMaterial::Secret(bytes) => Ok(Either::A(Buffer::from(bytes.clone()))),
                KeyMaterial::Ed25519Public(vk) => Ok(Either::A(Buffer::from(vk.as_bytes().to_vec()))),
                KeyMaterial::Ed25519Private(sk) => Ok(Either::A(Buffer::from(sk.to_bytes().to_vec()))),
                _ => Err(bad(format!("{} cannot be exported as raw", key.algorithm.name))),
            },
            other => Err(bad(format!("unsupported export format: {other}"))),
        }
    }

    #[napi]
    pub fn get_public_key(key: &CryptoKey) -> Result<CryptoKey> {
        if let Some(ref pub_key) = key.public_key {
            Ok((**pub_key).clone())
        } else {
            Err(bad("key has no public key"))
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
        let kind = match alg_kind(&name) {
            Some(k) => k,
            None => return false,
        };
        match operation.as_str() {
            "generateKey" => !matches!(kind, AlgKind::Kdf),
            "importKey" => true,
            "exportKey" => !matches!(kind, AlgKind::Kdf),
            "encrypt" | "decrypt" => {
                matches!(kind, AlgKind::Aes | AlgKind::ChaCha | AlgKind::RsaOaep)
            }
            "sign" | "verify" => {
                matches!(
                    kind,
                    AlgKind::Mac
                        | AlgKind::Ecdsa
                        | AlgKind::Ed25519
                        | AlgKind::RsaPss
                        | AlgKind::RsaPkcs1
                )
            }
            "digest" => true,
            "deriveBits" | "deriveKey" => {
                matches!(kind, AlgKind::Kdf | AlgKind::Ecdh | AlgKind::X25519)
            }
            "wrapKey" | "unwrapKey" => {
                matches!(kind, AlgKind::Aes | AlgKind::RsaOaep)
            }
            "getPublicKey" => !is_symmetric(kind) && !matches!(kind, AlgKind::Kdf),
            _ => false,
        }
    }

    #[napi(js_name = "encrypt")]
    pub fn encrypt_static(
        algorithm: CryptoOpParams,
        key: &CryptoKey,
        data: Uint8Array,
    ) -> Result<Buffer> {
        let name = algorithm
            .name
            .clone()
            .ok_or_else(|| bad("encrypt requires algorithm.name"))?;
        let iv = algorithm
            .iv
            .as_ref()
            .map(|u| u.as_ref().to_vec())
            .ok_or_else(|| bad("iv is required for encryption"))?;

        match (&key.material, name.as_str()) {
            (KeyMaterial::Secret(k), "AES-GCM") => {
                let ciphertext = if k.len() == 16 {
                    let cipher = Aes128Gcm::new_from_slice(k).map_err(|e| bad(e.to_string()))?;
                    cipher
                        .encrypt(GcmNonce::from_slice(&iv), data.as_ref())
                        .map_err(|e| bad(e.to_string()))?
                } else if k.len() == 32 {
                    let cipher = Aes256Gcm::new_from_slice(k).map_err(|e| bad(e.to_string()))?;
                    cipher
                        .encrypt(GcmNonce::from_slice(&iv), data.as_ref())
                        .map_err(|e| bad(e.to_string()))?
                } else {
                    return Err(bad("invalid AES-GCM key length"));
                };
                Ok(Buffer::from(ciphertext))
            }
            (KeyMaterial::Secret(k), "AES-CBC") => {
                let ciphertext = aes_cbc_encrypt(k, &iv, data.as_ref())?;
                Ok(Buffer::from(ciphertext))
            }
            (KeyMaterial::Secret(k), "ChaCha20-Poly1305") => {
                let cipher = ChaCha20Poly1305::new(ChaChaKey::from_slice(k));
                let ciphertext = cipher
                    .encrypt(ChaChaNonce::from_slice(&iv), data.as_ref())
                    .map_err(|e| bad(e.to_string()))?;
                Ok(Buffer::from(ciphertext))
            }
            _ => Err(bad(format!("encryption not supported for algorithm: {name}"))),
        }
    }

    #[napi(js_name = "decrypt")]
    pub fn decrypt_static(
        algorithm: CryptoOpParams,
        key: &CryptoKey,
        data: Uint8Array,
    ) -> Result<Buffer> {
        let name = algorithm
            .name
            .clone()
            .ok_or_else(|| bad("decrypt requires algorithm.name"))?;
        let iv = algorithm
            .iv
            .as_ref()
            .map(|u| u.as_ref().to_vec())
            .ok_or_else(|| bad("iv is required for decryption"))?;

        match (&key.material, name.as_str()) {
            (KeyMaterial::Secret(k), "AES-GCM") => {
                let plaintext = if k.len() == 16 {
                    let cipher = Aes128Gcm::new_from_slice(k).map_err(|e| bad(e.to_string()))?;
                    cipher
                        .decrypt(GcmNonce::from_slice(&iv), data.as_ref())
                        .map_err(|e| bad(e.to_string()))?
                } else if k.len() == 32 {
                    let cipher = Aes256Gcm::new_from_slice(k).map_err(|e| bad(e.to_string()))?;
                    cipher
                        .decrypt(GcmNonce::from_slice(&iv), data.as_ref())
                        .map_err(|e| bad(e.to_string()))?
                } else {
                    return Err(bad("invalid AES-GCM key length"));
                };
                Ok(Buffer::from(plaintext))
            }
            (KeyMaterial::Secret(k), "AES-CBC") => {
                let plaintext = aes_cbc_decrypt(k, &iv, data.as_ref())?;
                Ok(Buffer::from(plaintext))
            }
            (KeyMaterial::Secret(k), "ChaCha20-Poly1305") => {
                let cipher = ChaCha20Poly1305::new(ChaChaKey::from_slice(k));
                let plaintext = cipher
                    .decrypt(ChaChaNonce::from_slice(&iv), data.as_ref())
                    .map_err(|e| bad(e.to_string()))?;
                Ok(Buffer::from(plaintext))
            }
            _ => Err(bad(format!("decryption not supported for algorithm: {name}"))),
        }
    }

    #[napi(js_name = "sign")]
    pub fn sign_static(
        algorithm: CryptoOpParams,
        key: &CryptoKey,
        data: Uint8Array,
    ) -> Result<Buffer> {
        let name = algorithm
            .name
            .clone()
            .ok_or_else(|| bad("sign requires algorithm.name"))?;

        match (&key.material, name.as_str()) {
            (KeyMaterial::Secret(k), "HMAC") => {
                let hash_str = hash_name(&algorithm.hash);
                let tag = match hash_str.as_str() {
                    "SHA-1" => {
                        let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(k).map_err(|e| bad(e.to_string()))?;
                        mac.update(data.as_ref());
                        mac.finalize().into_bytes().to_vec()
                    }
                    "SHA-384" => {
                        let mut mac = <Hmac<Sha384> as Mac>::new_from_slice(k).map_err(|e| bad(e.to_string()))?;
                        mac.update(data.as_ref());
                        mac.finalize().into_bytes().to_vec()
                    }
                    "SHA-512" => {
                        let mut mac = <Hmac<Sha512> as Mac>::new_from_slice(k).map_err(|e| bad(e.to_string()))?;
                        mac.update(data.as_ref());
                        mac.finalize().into_bytes().to_vec()
                    }
                    _ => {
                        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(k).map_err(|e| bad(e.to_string()))?;
                        mac.update(data.as_ref());
                        mac.finalize().into_bytes().to_vec()
                    }
                };
                Ok(Buffer::from(tag))
            }
            (KeyMaterial::Ed25519Private(sk), "Ed25519") => {
                let sig = sk.sign(data.as_ref());
                Ok(Buffer::from(sig.to_bytes().to_vec()))
            }
            (KeyMaterial::EcPrivate(sk), "ECDSA") => {
                let signing_key = p256::ecdsa::SigningKey::from(sk);
                let sig: p256::ecdsa::Signature = p256::ecdsa::signature::Signer::sign(&signing_key, data.as_ref());
                Ok(Buffer::from(sig.to_bytes().to_vec()))
            }
            (KeyMaterial::RsaPrivate(sk), "RSASSA-PKCS1-v1_5") => {
                let signing_key = rsa::pkcs1v15::SigningKey::<Sha256>::new(sk.clone());
                let sig = signing_key.sign(data.as_ref());
                Ok(Buffer::from(sig.to_vec()))
            }
            _ => Err(bad(format!("signing not supported for algorithm: {name}"))),
        }
    }

    #[napi(js_name = "verify")]
    pub fn verify_static(
        algorithm: CryptoOpParams,
        key: &CryptoKey,
        signature: Uint8Array,
        data: Uint8Array,
    ) -> Result<bool> {
        let name = algorithm
            .name
            .clone()
            .ok_or_else(|| bad("verify requires algorithm.name"))?;

        match (&key.material, name.as_str()) {
            (KeyMaterial::Secret(_), "HMAC") => {
                let expected = Self::sign_static(algorithm, key, data)?;
                Ok(expected.as_ref() == signature.as_ref())
            }
            (KeyMaterial::Ed25519Public(vk), "Ed25519") => {
                let sig_bytes: [u8; 64] = match signature.as_ref().try_into() {
                    Ok(b) => b,
                    Err(_) => return Ok(false),
                };
                let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);
                Ok(vk.verify_strict(data.as_ref(), &sig).is_ok())
            }
            (KeyMaterial::Ed25519Private(sk), "Ed25519") => {
                let vk = sk.verifying_key();
                let sig_bytes: [u8; 64] = match signature.as_ref().try_into() {
                    Ok(b) => b,
                    Err(_) => return Ok(false),
                };
                let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);
                Ok(vk.verify_strict(data.as_ref(), &sig).is_ok())
            }
            (KeyMaterial::EcPublic(pk), "ECDSA") => {
                let verifying_key = p256::ecdsa::VerifyingKey::from(pk);
                if let Ok(sig) = p256::ecdsa::Signature::from_slice(signature.as_ref()) {
                    Ok(verifying_key.verify(data.as_ref(), &sig).is_ok())
                } else {
                    Ok(false)
                }
            }
            (KeyMaterial::EcPrivate(sk), "ECDSA") => {
                let verifying_key = p256::ecdsa::VerifyingKey::from(sk.public_key());
                if let Ok(sig) = p256::ecdsa::Signature::from_slice(signature.as_ref()) {
                    Ok(verifying_key.verify(data.as_ref(), &sig).is_ok())
                } else {
                    Ok(false)
                }
            }
            (KeyMaterial::RsaPublic(pk), "RSASSA-PKCS1-v1_5") => {
                let verifying_key = rsa::pkcs1v15::VerifyingKey::<Sha256>::new(pk.clone());
                if let Ok(sig) = rsa::pkcs1v15::Signature::try_from(signature.as_ref()) {
                    Ok(verifying_key.verify(data.as_ref(), &sig).is_ok())
                } else {
                    Ok(false)
                }
            }
            (KeyMaterial::RsaPrivate(sk), "RSASSA-PKCS1-v1_5") => {
                let verifying_key = rsa::pkcs1v15::VerifyingKey::<Sha256>::new(sk.to_public_key());
                if let Ok(sig) = rsa::pkcs1v15::Signature::try_from(signature.as_ref()) {
                    Ok(verifying_key.verify(data.as_ref(), &sig).is_ok())
                } else {
                    Ok(false)
                }
            }
            _ => Err(bad(format!("verification not supported for algorithm: {name}"))),
        }
    }

    #[napi(js_name = "deriveBits")]
    pub fn derive_bits_static(
        algorithm: CryptoOpParams,
        base_key: &CryptoKey,
        length: Option<u32>,
    ) -> Result<Buffer> {
        let name = algorithm
            .name
            .clone()
            .ok_or_else(|| bad("deriveBits requires algorithm.name"))?;

        match (&base_key.material, name.as_str()) {
            (KeyMaterial::Secret(k), "PBKDF2") => {
                let salt = algorithm
                    .salt
                    .as_ref()
                    .map(|u| u.as_ref().to_vec())
                    .ok_or_else(|| bad("PBKDF2 requires salt"))?;
                let iterations = algorithm.iterations.unwrap_or(100_000);
                let len = (length.ok_or_else(|| bad("PBKDF2 length required"))? / 8) as usize;
                let mut out = vec![0u8; len];
                pbkdf2::pbkdf2_hmac::<Sha256>(k, &salt, iterations, &mut out);
                Ok(Buffer::from(out))
            }
            (KeyMaterial::Secret(k), "HKDF") => {
                let salt = algorithm.salt.as_ref().map(|u| u.as_ref().to_vec());
                let info = algorithm.info.as_ref().map(|u| u.as_ref().to_vec());
                let len = (length.ok_or_else(|| bad("HKDF length required"))? / 8) as usize;
                let hk = hkdf::Hkdf::<Sha256>::new(salt.as_deref(), k);
                let mut out = vec![0u8; len];
                hk.expand(info.as_deref().unwrap_or(&[]), &mut out)
                    .map_err(|e| bad(e.to_string()))?;
                Ok(Buffer::from(out))
            }
            (KeyMaterial::X25519Private(sk, _), "X25519") => {
                let peer = algorithm
                    .public
                    .as_ref()
                    .ok_or_else(|| bad("X25519 deriveBits requires peer public key"))?;
                let peer_pk = match &peer.material {
                    KeyMaterial::X25519Public(pk) => pk,
                    KeyMaterial::X25519Private(_, pk) => pk,
                    _ => return Err(bad("peer is not an X25519 public key")),
                };
                let shared = sk.diffie_hellman(peer_pk);
                Ok(Buffer::from(shared.as_bytes().to_vec()))
            }
            _ => Err(bad(format!("deriveBits not supported for algorithm: {name}"))),
        }
    }

    #[napi(js_name = "deriveKey")]
    pub fn derive_key_static(
        algorithm: CryptoOpParams,
        base_key: &CryptoKey,
        derived_key_type: CryptoOpParams,
        extractable: bool,
        key_usages: Vec<String>,
    ) -> Result<CryptoKey> {
        let name = derived_key_type
            .name
            .clone()
            .ok_or_else(|| bad("deriveKey requires derivedKeyType.name"))?;
        let bits = derived_key_type.length.unwrap_or(256);
        let raw = Self::derive_bits_static(algorithm, base_key, Some(bits))?;
        let env = Env::from_raw(std::ptr::null_mut());
        Self::import_key(
            env,
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
    pub fn digest_static(algorithm: CryptoOpParams, data: Uint8Array) -> Result<Buffer> {
        let name = algorithm
            .name
            .clone()
            .ok_or_else(|| bad("digest requires algorithm.name"))?;

        let hash_bytes = match name.as_str() {
            "SHA-1" => Sha1::digest(data.as_ref()).to_vec(),
            "SHA-384" => Sha384::digest(data.as_ref()).to_vec(),
            "SHA-512" => Sha512::digest(data.as_ref()).to_vec(),
            _ => Sha256::digest(data.as_ref()).to_vec(),
        };
        Ok(Buffer::from(hash_bytes))
    }

    #[napi(js_name = "wrapKey")]
    pub fn wrap_key_static(
        format: String,
        key: &CryptoKey,
        wrapping_key: &CryptoKey,
        algorithm: Either<String, CryptoOpParams>,
    ) -> Result<Buffer> {
        let name = match &algorithm {
            Either::A(s) => s.clone(),
            Either::B(p) => p.name.clone().unwrap_or_default(),
        };

        let exported = match Self::export_key(format, key)? {
            Either::A(b) => b.to_vec(),
            Either::B(v) => serde_json::to_vec(&v).map_err(|e| bad(e.to_string()))?,
        };

        match (&wrapping_key.material, name.as_str()) {
            (KeyMaterial::Secret(k), "AES-KW") => {
                let wrapped = aes_kw_wrap(k, &exported)?;
                Ok(Buffer::from(wrapped))
            }
            _ => Err(bad(format!("wrapKey not supported for algorithm: {name}"))),
        }
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
    ) -> Result<CryptoKey> {
        let name = match &wrap_algorithm {
            Either::A(s) => s.clone(),
            Either::B(p) => p.name.clone().unwrap_or_default(),
        };

        let unwrapped = match (&unwrapping_key.material, name.as_str()) {
            (KeyMaterial::Secret(k), "AES-KW") => aes_kw_unwrap(k, wrapped_key.as_ref())?,
            _ => return Err(bad(format!("unwrapKey not supported for algorithm: {name}"))),
        };

        let env = Env::from_raw(std::ptr::null_mut());
        if format == "jwk" {
            let jwk_str = String::from_utf8(unwrapped).map_err(|e| bad(e.to_string()))?;
            let val: serde_json::Value = serde_json::from_str(&jwk_str).map_err(|e| bad(e.to_string()))?;
            let oct = val.get("k").and_then(|v| v.as_str()).ok_or_else(|| bad("invalid JWK"))?;
            let secret = base64_url_decode(oct)?;
            Self::import_key(
                env,
                "raw".to_string(),
                Either::A(Uint8Array::new(secret)),
                Either::B(derived_key_algorithm),
                extractable,
                key_usages,
            )
        } else {
            Self::import_key(
                env,
                format,
                Either::A(Uint8Array::new(unwrapped)),
                Either::B(derived_key_algorithm),
                extractable,
                key_usages,
            )
        }
    }

    // --- Instance Methods ---

    #[napi]
    pub fn encrypt(&self, algorithm: CryptoOpParams, key: &CryptoKey, data: Uint8Array) -> Result<Buffer> {
        Self::encrypt_static(algorithm, key, data)
    }

    #[napi]
    pub fn decrypt(&self, algorithm: CryptoOpParams, key: &CryptoKey, data: Uint8Array) -> Result<Buffer> {
        Self::decrypt_static(algorithm, key, data)
    }

    #[napi]
    pub fn sign(&self, algorithm: CryptoOpParams, key: &CryptoKey, data: Uint8Array) -> Result<Buffer> {
        Self::sign_static(algorithm, key, data)
    }

    #[napi]
    pub fn verify(
        &self,
        algorithm: CryptoOpParams,
        key: &CryptoKey,
        signature: Uint8Array,
        data: Uint8Array,
    ) -> Result<bool> {
        Self::verify_static(algorithm, key, signature, data)
    }

    #[napi]
    pub fn derive_bits(
        &self,
        algorithm: CryptoOpParams,
        base_key: &CryptoKey,
        length: Option<u32>,
    ) -> Result<Buffer> {
        Self::derive_bits_static(algorithm, base_key, length)
    }

    #[napi]
    pub fn derive_key(
        &self,
        algorithm: CryptoOpParams,
        base_key: &CryptoKey,
        derived_key_type: CryptoOpParams,
        extractable: bool,
        key_usages: Vec<String>,
    ) -> Result<CryptoKey> {
        Self::derive_key_static(algorithm, base_key, derived_key_type, extractable, key_usages)
    }

    #[napi]
    pub fn wrap_key(
        &self,
        format: String,
        key: &CryptoKey,
        wrapping_key: &CryptoKey,
        algorithm: Either<String, CryptoOpParams>,
    ) -> Result<Buffer> {
        Self::wrap_key_static(format, key, wrapping_key, algorithm)
    }

    #[napi]
    pub fn unwrap_key(
        &self,
        format: String,
        wrapped_key: Uint8Array,
        unwrapping_key: &CryptoKey,
        wrap_algorithm: Either<String, CryptoOpParams>,
        derived_key_algorithm: CryptoKeyAlgorithm,
        extractable: bool,
        key_usages: Vec<String>,
    ) -> Result<CryptoKey> {
        Self::unwrap_key_static(
            format,
            wrapped_key,
            unwrapping_key,
            wrap_algorithm,
            derived_key_algorithm,
            extractable,
            key_usages,
        )
    }

    #[napi]
    pub fn digest(&self, algorithm: CryptoOpParams, data: Uint8Array) -> Result<Buffer> {
        Self::digest_static(algorithm, data)
    }

    #[napi(js_name = "encapsulateBits")]
    pub fn encapsulate_bits_static(_algorithm: CryptoOpParams, _public_key: &CryptoKey) -> Result<Buffer> {
        Err(bad("encapsulateBits (ML-KEM) is not implemented"))
    }

    #[napi(js_name = "decapsulateBits")]
    pub fn decapsulate_bits_static(
        _algorithm: CryptoOpParams,
        _private_key: &CryptoKey,
        _ciphertext: Uint8Array,
    ) -> Result<Buffer> {
        Err(bad("decapsulateBits (ML-KEM) is not implemented"))
    }

    #[napi(js_name = "encapsulateKey")]
    pub fn encapsulate_key_static(
        _algorithm: CryptoOpParams,
        _public_key: &CryptoKey,
        _derived_key_type: CryptoOpParams,
        _extractable: bool,
        _key_usages: Vec<String>,
    ) -> Result<CryptoKey> {
        Err(bad("encapsulateKey (ML-KEM) is not implemented"))
    }

    #[napi(js_name = "decapsulateKey")]
    pub fn decapsulate_key_static(
        _algorithm: CryptoOpParams,
        _private_key: &CryptoKey,
        _ciphertext: Uint8Array,
        _derived_key_type: CryptoOpParams,
        _extractable: bool,
        _key_usages: Vec<String>,
    ) -> Result<CryptoKey> {
        Err(bad("decapsulateKey (ML-KEM) is not implemented"))
    }

    #[napi(js_name = "encapsulateBits")]
    pub fn encapsulate_bits(&self, algorithm: CryptoOpParams, public_key: &CryptoKey) -> Result<Buffer> {
        Self::encapsulate_bits_static(algorithm, public_key)
    }

    #[napi(js_name = "decapsulateBits")]
    pub fn decapsulate_bits(
        &self,
        algorithm: CryptoOpParams,
        private_key: &CryptoKey,
        ciphertext: Uint8Array,
    ) -> Result<Buffer> {
        Self::decapsulate_bits_static(algorithm, private_key, ciphertext)
    }

    #[napi(js_name = "encapsulateKey")]
    pub fn encapsulate_key(
        &self,
        algorithm: CryptoOpParams,
        public_key: &CryptoKey,
        derived_key_type: CryptoOpParams,
        extractable: bool,
        key_usages: Vec<String>,
    ) -> Result<CryptoKey> {
        Self::encapsulate_key_static(algorithm, public_key, derived_key_type, extractable, key_usages)
    }

    #[napi(js_name = "decapsulateKey")]
    pub fn decapsulate_key(
        &self,
        algorithm: CryptoOpParams,
        private_key: &CryptoKey,
        ciphertext: Uint8Array,
        derived_key_type: CryptoOpParams,
        extractable: bool,
        key_usages: Vec<String>,
    ) -> Result<CryptoKey> {
        Self::decapsulate_key_static(
            algorithm,
            private_key,
            ciphertext,
            derived_key_type,
            extractable,
            key_usages,
        )
    }
}

fn base64_url_encode(data: &[u8]) -> String {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        let n = (u32::from(data[i]) << 16) | (u32::from(data[i + 1]) << 8) | u32::from(data[i + 2]);
        out.push(B64[(n >> 18) as usize] as char);
        out.push(B64[((n >> 12) & 0x3f) as usize] as char);
        out.push(B64[((n >> 6) & 0x3f) as usize] as char);
        out.push(B64[(n & 0x3f) as usize] as char);
        i += 3;
    }
    let rem = data.len() - i;
    if rem == 1 {
        let n = u32::from(data[i]) << 16;
        out.push(B64[(n >> 18) as usize] as char);
        out.push(B64[((n >> 12) & 0x3f) as usize] as char);
    } else if rem == 2 {
        let n = (u32::from(data[i]) << 16) | (u32::from(data[i + 1]) << 8);
        out.push(B64[(n >> 18) as usize] as char);
        out.push(B64[((n >> 12) & 0x3f) as usize] as char);
        out.push(B64[((n >> 6) & 0x3f) as usize] as char);
    }
    out
}

fn base64_url_decode(s: &str) -> Result<Vec<u8>> {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut rev = [255u8; 256];
    for (i, &c) in B64.iter().enumerate() {
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
            if c == b'=' {
                i += 1;
                continue;
            }
            let v = rev[c as usize];
            if v == 255 {
                return Err(bad("invalid base64url character"));
            }
            buf[cnt] = v;
            cnt += 1;
            i += 1;
        }
        if cnt == 0 {
            break;
        }
        if cnt == 4 {
            let n = (u32::from(buf[0]) << 18)
                | (u32::from(buf[1]) << 12)
                | (u32::from(buf[2]) << 6)
                | u32::from(buf[3]);
            out.push((n >> 16) as u8);
            out.push((n >> 8) as u8);
            out.push(n as u8);
        } else if cnt == 3 {
            let n =
                (u32::from(buf[0]) << 18) | (u32::from(buf[1]) << 12) | (u32::from(buf[2]) << 6);
            out.push((n >> 16) as u8);
            out.push((n >> 8) as u8);
        } else if cnt == 2 {
            let n = (u32::from(buf[0]) << 18) | (u32::from(buf[1]) << 12);
            out.push((n >> 16) as u8);
        } else {
            return Err(bad("invalid base64url length"));
        }
    }
    Ok(out)
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
    pub fn new(key_format: Either<String, Unknown>) -> Result<Self> {
        let format = match key_format {
            Either::A(s) => s,
            Either::B(u) => u.coerce_to_string()?.into_utf8()?.into_owned()?,
        };
        Ok(Self {
            format,
            bytes: Vec::new(),
        })
    }

    #[napi(getter)]
    pub fn format(&self) -> String {
        self.format.clone()
    }

    #[napi(js_name = "keyFormat", getter)]
    pub fn key_format(&self) -> String {
        self.format.clone()
    }

    #[napi]
    pub fn set_bytes(&mut self, bytes: Buffer) {
        self.bytes = bytes.as_ref().to_vec();
    }

    #[napi(getter)]
    pub fn bytes(&self) -> Buffer {
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
