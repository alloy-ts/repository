use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chacha20poly1305::ChaCha20Poly1305;
use chacha20poly1305::aead::{Aead, Key, KeyInit, Nonce};
use multi_sig::DataView as _;
use multi_util::CodecInfo as _;
use napi::Result;
use napi::bindgen_prelude::Buffer;
use napi::bindgen_prelude::{Either, Either6};
use napi_derive::napi;
use rand::Rng;
use thiserror::Error;

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
/// Key size in bytes (256 bits).
pub const KEY_SIZE: usize = 32;
/// Nonce size in bytes (96 bits, ChaCha20-Poly1305 standard).
pub const NONCE_SIZE: usize = 12;
/// Magic header identifying crypt-encrypted payloads.
const MAGIC_HEADER: &[u8] = b"GITCRYPT";

/// A symmetric ChaCha20-Poly1305 key. Exposed to JS as the `SymmetricCryptoKey` class.
#[napi(js_name = "SymmetricCryptoKey")]
#[derive(Clone)]
pub struct SymmetricCryptoKey {
    key: [u8; KEY_SIZE],
}

#[napi]
impl SymmetricCryptoKey {
    #[napi(constructor)]
    pub fn new(bytes: Option<&[u8]>) -> Result<Self> {
        match bytes {
            Some(bytes) => Ok(Self::from_bytes(bytes)?),
            None => Ok(Self::generate_random()),
        }
    }

    /// Generate a new random key from the OS CSPRNG.
    #[napi(factory)]
    pub fn generate_random() -> Self {
        let mut key = [0u8; KEY_SIZE];
        rand::rng().fill_bytes(&mut key);
        Self { key }
    }

    /// Build a key from raw bytes, rejecting any length other than [`KEY_SIZE`].
    #[napi(factory)]
    pub fn from_bytes(bytes: &[u8]) -> CryptoResult<Self> {
        if bytes.len() != KEY_SIZE {
            return Err(CryptoError::InvalidKeyFormat);
        }
        let mut key = [0u8; KEY_SIZE];
        key.copy_from_slice(bytes);
        Ok(Self { key })
    }

    /// Borrow the raw key bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.key
    }

    /// Encrypt `plaintext` into `MAGIC_HEADER || nonce || ciphertext+tag` (ChaCha20-Poly1305).
    pub fn encrypt(&self, plaintext: &[u8]) -> CryptoResult<Vec<u8>> {
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        rand::rng().fill_bytes(&mut nonce_bytes);

        let cipher = ChaCha20Poly1305::new(Key::<ChaCha20Poly1305>::from_slice(&self.key));
        let ciphertext = cipher
            .encrypt(
                Nonce::<ChaCha20Poly1305>::from_slice(&nonce_bytes),
                plaintext,
            )
            .map_err(|_| CryptoError::Crypto("encryption failed".into()))?;

        let mut result = Vec::with_capacity(MAGIC_HEADER.len() + NONCE_SIZE + ciphertext.len());
        result.extend_from_slice(MAGIC_HEADER);
        result.extend_from_slice(&nonce_bytes);
        result.extend_from_slice(&ciphertext);
        Ok(result)
    }

    /// Decrypt a payload produced by [`SymmetricCryptoKey::encrypt`].
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

    /// Whether `data` carries the crypt magic header.
    pub fn is_encrypted(data: &[u8]) -> bool {
        data.len() >= MAGIC_HEADER.len() && &data[..MAGIC_HEADER.len()] == MAGIC_HEADER
    }
}

// ---------------------------------------------------------------------------
// KeyManager re-exported from vault.rs
// ---------------------------------------------------------------------------

pub use crate::vault::KeyManager;

/// Lowercase hex encoding, without pulling in an extra dependency.
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
    /// Create from raw bytes.
    #[napi(constructor)]
    pub fn new(bytes: Buffer) -> Self {
        Self {
            bytes: bytes.as_ref().to_vec(),
        }
    }

    /// The raw bytes.
    #[napi(getter)]
    pub fn bytes(&self) -> Buffer {
        Buffer::from(self.bytes.clone())
    }

    /// Alias for [`SignatureBytes::bytes`].
    #[napi]
    pub fn as_bytes(&self) -> Buffer {
        Buffer::from(self.bytes.clone())
    }

    /// Length in bytes.
    #[napi(getter)]
    pub fn length(&self) -> u32 {
        self.bytes.len() as u32
    }

    /// Whether the signature is empty.
    #[napi(getter)]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Lowercase hex form.
    #[napi(getter)]
    pub fn hex(&self) -> String {
        hex_lower(&self.bytes)
    }
}

/// A signature scheme identifier. Mirrors `multi_sig::types::SignatureScheme`.
#[napi]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignatureScheme {
    code: u32,
    name: String,
}

#[napi]
impl SignatureScheme {
    /// Create from a multicodec name (e.g. `"eddsa-msig"`) or numeric codec value.
    #[napi(factory)]
    pub fn from_codec(codec: Either<String, u32>) -> Result<Self> {
        let c = match codec {
            Either::A(name) => multi_codec::Codec::try_from(name.as_str())
                .map_err(|e| napi::Error::from_reason(format!("unknown codec '{name}': {e}")))?,
            Either::B(code) => multi_codec::Codec::try_from(code)
                .map_err(|e| napi::Error::from_reason(format!("unknown codec {code}: {e}")))?,
        };
        Ok(Self {
            code: c.code() as u32,
            name: c.as_str().to_string(),
        })
    }

    /// The multicodec name, e.g. `"eddsa-msig"`.
    #[napi(getter)]
    pub fn name(&self) -> String {
        self.name.clone()
    }

    /// The multicodec numeric value.
    #[napi(getter)]
    pub fn code(&self) -> u32 {
        self.code
    }
}

/// A self-describing multisignature.
#[napi]
#[derive(Clone)]
pub struct MultiSignature {
    inner: multi_sig::Multisig,
    key_format: Option<String>,
}

#[napi]
impl MultiSignature {
    /// Decode a MultiSignature from its binary wire form.
    #[napi(constructor)]
    pub fn new(bytes: Buffer, key_format: Option<String>) -> Result<Self> {
        multi_sig::Multisig::try_from(bytes.as_ref())
            .map(|inner| Self { inner, key_format })
            .map_err(|e| napi::Error::from_reason(format!("decode multisig: {e}")))
    }

    /// The key format this signature is associated with (e.g. `CryptoKey.Format.JWK`),
    /// when supplied at construction time. Otherwise `undefined`.
    #[napi(getter, js_name = "keyFormat")]
    pub fn key_format(&self) -> Option<String> {
        self.key_format.clone()
    }

    #[napi]
    pub fn from_bytes(&self) -> Buffer {
        Buffer::from(Vec::from(self.inner.clone()))
    }

    /// The base-encoded string form (lowercase hex, with the multibase `f` prefix) — identical to
    /// what [`Signature::try_build_encoded`] produces.
    #[napi]
    pub fn from_hex(&self) -> String {
        let encoded: multi_util::BaseEncoded<multi_sig::Multisig> =
            multi_util::BaseEncoded::from(self.inner.clone());
        encoded.to_string()
    }

    /// The signature codec name, e.g. `"eddsa-msig"`.
    #[napi(getter)]
    pub fn codec(&self) -> String {
        self.inner.codec().as_str().to_string()
    }

    /// The signature codec numeric value.
    #[napi(getter)]
    pub fn code(&self) -> u32 {
        self.inner.codec().code() as u32
    }

    /// The raw signature bytes.
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

    /// The raw signature bytes (alias of [`MultiSignature::signature_bytes`] for property access).
    #[napi(getter)]
    pub fn bytes(&self) -> Buffer {
        Buffer::from(Vec::from(self.inner.clone()))
    }

    /// The in-band signed message (empty for a detached signature).
    #[napi(getter)]
    pub fn message(&self) -> Buffer {
        Buffer::from(self.inner.message.clone())
    }

    /// True when the message travels in-band (combined signature).
    #[napi(getter)]
    pub fn is_combined(&self) -> bool {
        !self.inner.message.is_empty()
    }

    /// The one-time leaf index consumed (XMSS stateful signatures), if present.
    #[napi(getter)]
    pub fn sig_index(&self) -> Option<u32> {
        self.inner.sig_index()
    }

    /// The Merkle tree depth (Merkle-Lamport), if present.
    #[napi(getter)]
    pub fn depth(&self) -> Option<u8> {
        self.inner.depth()
    }
}

/// Resolve an `Either<String, u32>` codec argument into a `multi_codec::Codec`.
fn resolve_codec(codec: Either<String, u32>) -> Result<multi_codec::Codec> {
    match codec {
        Either::A(name) => multi_codec::Codec::try_from(name.as_str())
            .map_err(|e| napi::Error::from_reason(format!("unknown codec '{name}': {e}"))),
        Either::B(code) => multi_codec::Codec::try_from(code)
            .map_err(|e| napi::Error::from_reason(format!("unknown codec {code}: {e}"))),
    }
}

#[napi(object)]
pub struct Signature {
    inner: multi_sig::Builder,
}

#[napi(object)]
impl Signature {
    /// Create a builder for `codec` — a multicodec name (`"eddsa-msig"`) or numeric value.
    #[napi(constructor)]
    pub fn new(codec: Either<String, u32>) -> Result<Self> {
        let c = resolve_codec(codec)?;
        Ok(Self {
            inner: multi_sig::Builder::new(c),
        })
    }

    /// Set the message payload (makes a combined signature).
    #[napi]
    pub fn with_message(&mut self, message: Buffer) -> &Self {
        self.inner = std::mem::take(&mut self.inner).with_message_bytes(&message);
        self
    }

    /// Set the raw signature bytes.
    #[napi]
    pub fn with_signature(&mut self, signature: Buffer) -> &Self {
        self.inner = std::mem::take(&mut self.inner).with_signature_bytes(&signature);
        self
    }

    /// Set the payload encoding codec (name or numeric value).
    #[napi]
    pub fn with_payload_encoding(&mut self, codec: Either<String, u32>) -> Result<&Self> {
        let c = resolve_codec(codec)?;
        self.inner = std::mem::take(&mut self.inner).with_payload_encoding(c);
        Ok(self)
    }

    /// Set the BLS scheme type: 0=Basic, 1=MessageAugmentation, 2=ProofOfPossession.
    #[napi]
    pub fn with_scheme(&mut self, scheme: u32) -> &Self {
        self.inner = std::mem::take(&mut self.inner).with_scheme(scheme as u8);
        self
    }

    /// Set the threshold value `t` (plaintext).
    #[napi]
    pub fn with_threshold(&mut self, threshold: u32) -> &Self {
        self.inner = std::mem::take(&mut self.inner).with_threshold(threshold as usize);
        self
    }

    /// Set the share count `n` (plaintext).
    #[napi]
    pub fn with_limit(&mut self, limit: u32) -> &Self {
        self.inner = std::mem::take(&mut self.inner).with_limit(limit as usize);
        self
    }

    /// Set the share identifier (32-byte BLS scalar).
    #[napi]
    pub fn with_identifier(&mut self, identifier: Buffer) -> &Self {
        self.inner = std::mem::take(&mut self.inner).with_identifier(&identifier);
        self
    }

    /// Set the accumulated threshold data blob.
    #[napi]
    pub fn with_threshold_data(&mut self, data: Buffer) -> &Self {
        self.inner = std::mem::take(&mut self.inner).with_threshold_data(&data);
        self
    }

    /// Set the XMSS one-time leaf index consumed by this signature.
    #[napi]
    pub fn with_sig_index(&mut self, index: u32) -> &Self {
        self.inner = std::mem::take(&mut self.inner).with_sig_index(index);
        self
    }

    /// Set the Merkle tree depth.
    #[napi]
    pub fn with_depth(&mut self, depth: u32) -> &Self {
        self.inner = std::mem::take(&mut self.inner).with_depth(depth as u8);
        self
    }

    /// Accumulate a threshold signature share (folded in by `try_build`).
    #[napi]
    pub fn add_signature_share(&mut self, share: &MultiSignature) -> &Self {
        self.inner = std::mem::take(&mut self.inner).add_signature_share(&share.inner);
        self
    }

    /// Set the threshold disclosure mode for a share being built.
    ///
    /// `mode`: 0 = Full (t/n plaintext), 1 = Partial (n plaintext, t encrypted),
    /// 2 = FullConfidential (both encrypted). `metaKey` (32 bytes from
    /// [`Signature::generateMetaKey`]) is required for modes 1 and 2.
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
                )));
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

    /// Construct from a BLS12-381 signature (`blsful::Signature`) with an explicit codec —
    /// `"bls12_381-g1-msig"` (48-byte sig) or `"bls12_381-g2-msig"` (96-byte sig).
    #[napi(factory)]
    pub fn new_from_bls_signature(codec: Either<String, u32>, signature: Buffer) -> Result<Self> {
        let c = resolve_codec(codec)?;
        let inner = match c {
            multi_codec::Codec::Bls12381G1Msig => {
                let sig = blsful::Signature::<blsful::Bls12381G1Impl>::try_from(signature.as_ref())
                    .map_err(|e| {
                        napi::Error::from_reason(format!("invalid BLS12-381 G1 signature: {e}"))
                    })?;
                multi_sig::Builder::new_from_bls_signature_with_codec(c, &sig)
                    .map_err(|e| napi::Error::from_reason(format!("bls signature: {e}")))?
            }
            multi_codec::Codec::Bls12381G2Msig => {
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
                )));
            }
        };
        Ok(Self { inner })
    }

    /// Construct from a BLS12-381 signature *share* (`blsful::SignatureShare`) with an explicit
    /// share codec — `"bls12_381-g1-share-msig"` or `"bls12_381-g2-share-msig"`.
    #[napi(factory)]
    pub fn new_from_bls_signature_share(
        codec: Either<String, u32>,
        threshold: u32,
        limit: u32,
        share: Buffer,
    ) -> Result<Self> {
        let c = resolve_codec(codec)?;
        let inner = match c {
            multi_codec::Codec::Bls12381G1ShareMsig => {
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
            multi_codec::Codec::Bls12381G2ShareMsig => {
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
                )));
            }
        };
        Ok(Self { inner })
    }

    /// Generate a fresh 32-byte threshold-metadata key, required by the Partial and
    /// FullConfidential disclosure modes.
    ///
    /// **Store it safely** — losing it makes `t` (and `n`) unrecoverable.
    #[napi]
    pub fn generate_meta_key() -> Buffer {
        Buffer::from(multi_sig::generate_meta_key().to_vec())
    }

    /// Build the [`MultiSignature`], folding in any accumulated shares.
    #[napi]
    pub fn try_build(&self) -> Result<MultiSignature> {
        self.inner
            .clone()
            .try_build()
            .map(|inner| MultiSignature {
                inner,
                key_format: None,
            })
            .map_err(|e| napi::Error::from_reason(format!("build multisig: {e}")))
    }

    /// Build and return the base-encoded (lowercase hex) string form.
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

/// Options controlling the git clean/smudge/diff filter registered by [`Crypto::init`].
///
/// Defaults keep the project's built-in `crypto` filter. To instead interoperate with the
/// upstream `git-crypt` CLI, set `filterName` to `"git-crypt"` and the commands to
/// `"git-crypt clean %f"` / `"git-crypt smudge %f"` / `"git-crypt diff"`.
///
/// **Caveat:** this crate's native [`Crypto::encrypt`]/[`Crypto::decrypt`] use
/// ChaCha20-Poly1305 and are *not* byte-compatible with upstream git-crypt (which uses
/// AES-CBC + HMAC), even though both share the `GITCRYPT` magic header. Setting the filter
/// name to `git-crypt` only makes sense when you actually run the upstream `git-crypt` binary
/// for the filter path; this crate's own key is then unused for filtering.
#[napi(object)]
pub struct CryptFilterOptions {
    /// The git filter name — the `<name>` in `filter.<name>`. Default `"crypto"`.
    pub filter_name: Option<String>,
    /// The `clean` command (worktree → git). Default `"crypto clean %f"`.
    pub clean: Option<String>,
    /// The `smudge` command (git → worktree). Default `"crypto smudge %f"`.
    pub smudge: Option<String>,
    /// The `diff` command. Default `"crypto diff"`.
    pub diff: Option<String>,
}

impl Default for CryptFilterOptions {
    fn default() -> Self {
        Self {
            filter_name: None,
            clean: None,
            smudge: None,
            diff: None,
        }
    }
}

/// Resolved, non-optional filter configuration.
struct ResolvedFilter {
    name: String,
    clean: String,
    smudge: String,
    diff: String,
}

impl CryptFilterOptions {
    /// Apply defaults for any unset field.
    fn resolve(&self) -> ResolvedFilter {
        ResolvedFilter {
            name: self
                .filter_name
                .clone()
                .unwrap_or_else(|| "crypto".to_string()),
            clean: self
                .clean
                .clone()
                .unwrap_or_else(|| "crypto clean %f".to_string()),
            smudge: self
                .smudge
                .clone()
                .unwrap_or_else(|| "crypto smudge %f".to_string()),
            diff: self
                .diff
                .clone()
                .unwrap_or_else(|| "crypto diff".to_string()),
        }
    }
}

impl Crypto {
    /// A cheap thread-local view of the repository.
    fn gix(&self) -> gix::Repository {
        thread_local(&self.repo)
    }

    /// A [`KeyManager`] rooted at this repository's git directory.
    fn key_manager(&self) -> KeyManager {
        KeyManager::new(self.gix().git_dir())
    }

    /// Load the default key, ensuring the repository is initialized first.
    fn load_key(&self) -> CryptoResult<SymmetricCryptoKey> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized);
        }
        km.load_key()
    }

    /// Path to the local git config file.
    fn config_path(&self) -> PathBuf {
        self.gix().git_dir().join("config")
    }

    /// Write the resolved clean/smudge/diff filter into the local git config for git-CLI
    /// interop. `f.name` is the `<name>` in `filter.<name>`.
    fn configure_filters(&self, f: &ResolvedFilter) -> CryptoResult<()> {
        let config_path = self.config_path();
        let mut config = if config_path.exists() {
            gix::config::File::from_path_no_includes(
                config_path.clone(),
                gix::config::Source::Local,
            )
            .map_err(|e| CryptoError::Git(e.to_string()))?
        } else {
            gix::config::File::new(gix::config::file::Metadata::from(
                gix::config::Source::Local,
            ))
        };

        let prefix = format!("filter.{}", f.name);
        config
            .set_raw_value(&format!("{prefix}.clean"), &f.clean)
            .map_err(|e| CryptoError::Git(e.to_string()))?;
        config
            .set_raw_value(&format!("{prefix}.smudge"), &f.smudge)
            .map_err(|e| CryptoError::Git(e.to_string()))?;
        config
            .set_raw_value(&format!("{prefix}.diff"), &f.diff)
            .map_err(|e| CryptoError::Git(e.to_string()))?;
        config
            .set_raw_value(&format!("{prefix}.required"), "true")
            .map_err(|e| CryptoError::Git(e.to_string()))?;

        let mut file = std::fs::File::create(&config_path)?;
        config.write_to(&mut file)?;
        Ok(())
    }

    /// Remove the `filter.<name>.*` section from the local git config.
    fn remove_filters(&self, name: &str) -> CryptoResult<()> {
        let config_path = self.config_path();
        if !config_path.exists() {
            return Ok(());
        }
        let mut config = gix::config::File::from_path_no_includes(
            config_path.clone(),
            gix::config::Source::Local,
        )
        .map_err(|e| CryptoError::Git(e.to_string()))?;
        let _ = config.remove_section("filter", Some(name.into()));
        let mut file = std::fs::File::create(&config_path)?;
        config.write_to(&mut file)?;
        Ok(())
    }
}

#[napi]
impl Crypto {
    /// Whether the repository has been initialized for crypt.
    #[napi]
    pub fn is_initialized(&self) -> bool {
        self.key_manager().is_initialized()
    }

    /// Path to the on-disk crypt key directory (`<git-dir>/crypt`).
    #[napi]
    pub fn key_dir(&self) -> String {
        self.key_manager()
            .git_crypt_dir()
            .to_string_lossy()
            .into_owned()
    }

    /// Generate a fresh random key and persist it as the default key, overwriting any existing
    /// default. Requires the repository to already be initialized.
    #[napi]
    pub fn generate_key(&self) -> Result<SymmetricCryptoKey> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized.into());
        }
        Ok(km.generate_key()?)
    }

    /// Initialize crypt: create the key directory, generate a key, and configure filters.
    /// Idempotent — a no-op if already initialized. `opts` overrides the filter name/commands
    /// (defaults to the built-in `crypto` filter).
    #[napi]
    pub fn init(&self, opts: Option<CryptFilterOptions>) -> Result<()> {
        let km = self.key_manager();
        if km.is_initialized() {
            return Ok(());
        }
        km.init_dirs()?;
        let _key = km.generate_key()?;
        self.configure_filters(&opts.unwrap_or_default().resolve())?;
        Ok(())
    }

    /// Lock the repository by removing the `filter.<name>.*` config (encrypted files then
    /// show their encrypted content on checkout). `opts.filterName` selects which section to
    /// remove (defaults to `crypto`).
    #[napi]
    pub fn lock(&self, opts: Option<CryptFilterOptions>) -> Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized.into());
        }
        let name = opts.unwrap_or_default().resolve().name;
        self.remove_filters(&name)?;
        Ok(())
    }

    /// Unlock the repository. If `key_file` is provided it is imported first, then the key
    /// is verified and the `filter.<name>.*` config is (re)written. `opts` overrides the
    /// filter name/commands (defaults to the built-in `crypto` filter).
    #[napi]
    pub fn unlock(&self, key_file: Option<String>, opts: Option<CryptFilterOptions>) -> Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized.into());
        }
        if let Some(kf) = key_file {
            km.import_key(Path::new(&kf))?;
        }
        let _key = km.load_key()?;
        self.configure_filters(&opts.unwrap_or_default().resolve())?;
        Ok(())
    }

    /// Export the default key to `output_path`.
    #[napi]
    pub fn export_key(&self, output_path: String) -> Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized.into());
        }
        km.export_key(Path::new(&output_path))?;
        Ok(())
    }

    /// Import a key from `input_path` as the default key.
    #[napi]
    pub fn import_key(&self, input_path: String) -> Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized.into());
        }
        km.import_key(Path::new(&input_path))?;
        Ok(())
    }

    /// Encrypt `data`, returning `MAGIC_HEADER || nonce || ciphertext+tag`.
    #[napi]
    pub fn encrypt(&self, data: Buffer) -> Result<Buffer> {
        let key = self.load_key()?;
        let out = key.encrypt(data.as_ref())?;
        Ok(Buffer::from(out))
    }

    /// Decrypt `data` produced by [`Crypto::encrypt`].
    #[napi]
    pub fn decrypt(&self, data: Buffer) -> Result<Buffer> {
        let key = self.load_key()?;
        let out = key.decrypt(data.as_ref())?;
        Ok(Buffer::from(out))
    }

    /// Whether `data` carries the crypt magic header.
    #[napi]
    pub fn is_encrypted(&self, data: Buffer) -> bool {
        SymmetricCryptoKey::is_encrypted(data.as_ref())
    }
}

/// Open the repository at `path` and return its git directory (used by the static,
/// repo-agnostic recipient-management helpers).
#[cfg(any(feature = "ssh", feature = "gpg"))]
fn open_repo_git_dir(path: &str) -> CryptoResult<PathBuf> {
    let repo = gix::open(path).map_err(|e| CryptoError::Git(format!("open repo: {e}")))?;
    Ok(repo.git_dir().to_path_buf())
}

#[cfg(feature = "ssh")]
#[napi]
impl Crypto {
    /// Add an SSH (ed25519) recipient (feature `ssh`). The repo's symmetric key is encrypted
    /// for the recipient's public key and written to `crypt/keys/ssh/<name>.ssh`.
    #[napi]
    pub fn add_ssh_user(&self, ssh_key_path: String, alias: Option<String>) -> Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized.into());
        }

        let key = km.load_key()?;
        let ssh_key = std::fs::read_to_string(&ssh_key_path)?;
        let encrypted_key = IdentityManager::encrypt_key_for_ssh_recipient(&key, &ssh_key)?;

        let name = alias
            .map(|s| sanitize_label(&s))
            .filter(|s: &String| !s.is_empty())
            .unwrap_or_else(|| derive_recipient_name(&ssh_key, Path::new(&ssh_key_path)));

        let ssh_dir = km.git_crypt_dir().join("keys").join("ssh");
        std::fs::create_dir_all(&ssh_dir)?;
        let key_file = ssh_dir.join(format!("{name}.ssh"));
        std::fs::write(&key_file, encrypted_key)?;
        Ok(())
    }

    /// Import an SSH-encrypted repo key using an OpenSSH ed25519 identity (feature `ssh`).
    /// When the identity is passphrase-protected, supply `passphrase`.
    #[napi]
    pub fn import_ssh_key(
        &self,
        encrypted_path: String,
        identity_path: String,
        passphrase: Option<String>,
    ) -> Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized.into());
        }

        let encrypted = std::fs::read(&encrypted_path)?;
        let identity = std::fs::read_to_string(&identity_path)?;
        let key = IdentityManager::decrypt_key_with_ssh_identity(
            &encrypted,
            &identity,
            passphrase.as_deref(),
        )?;
        km.save_key(&key)?;
        Ok(())
    }

    /// Store an SSH key in the ref-backed key vault (feature `ssh`). Accepts OpenSSH or PKCS#8
    /// PEM keys.
    ///
    /// A public-key input writes only `refs/keys/<name>.pub` (plaintext). A private-key input
    /// writes `refs/keys/<name>.pub` (derived, plaintext) and `refs/keys/<name>.pem` — the
    /// 32-byte seed encrypted at rest with AES-256-GCM under `SHA256(initial commit) || passphrase`.
    /// The default signing key lives at `refs/keys/sign.{pub,pem}`.
    ///
    /// With `opts.commitToTree` set (a full or short ref), the key material is additionally
    /// committed onto that branch as a new commit adding `.gitattributes`, `.<name>.pub` and the
    /// encrypted `.<name>.pem`. Default (`undefined`): refs only, no commit.
    #[napi]
    pub fn add_key(
        &self,
        name: String,
        key_pem: String,
        passphrase: Option<String>,
        opts: Option<KeyStoreOptions>,
    ) -> Result<()> {
        let r = thread_local(&self.repo);
        let pass = passphrase.unwrap_or_default();

        let is_private;
        // Public-key input → store only `.pub` (no encryption, nothing secret).
        if let Ok(public) = parse_ed25519_public(key_pem.trim()) {
            let pub_text = ed25519_public_openssh_text(&public)?;
            write_key_ref(&r, &format!("refs/keys/{name}.pub"), pub_text.as_bytes())?;
            is_private = false;
        } else {
            // Private-key input → store `.pub` (derived, plaintext) + `.pem` (the seed encrypted
            // with the repo's *symmetric* crypto key). Using the symmetric key keeps the
            // committed `.pem` consistent with the `**.pem` crypto filter (so `smudge` works on
            // checkout) and means anyone holding the repo key (via unlock / addSshUser) can sign.
            let (seed, public) = parse_ed25519_private(key_pem.trim(), &pass)?;
            let pub_text = ed25519_public_openssh_text(&public)?;
            let enc = self.load_key()?.encrypt(&seed)?;
            write_key_ref(&r, &format!("refs/keys/{name}.pem"), &enc)?;
            write_key_ref(&r, &format!("refs/keys/{name}.pub"), pub_text.as_bytes())?;
            is_private = true;
        }

        // Optionally commit the key material onto a branch (versioned vault).
        if let Some(target) = opts.as_ref().and_then(|o| o.commit_to_tree.clone()) {
            let patterns = opts
                .as_ref()
                .and_then(|o| o.gitattributes.clone())
                .unwrap_or_else(default_gitattributes_patterns);
            let filter_name = opts
                .as_ref()
                .and_then(|o| o.filter_name.clone())
                .unwrap_or_else(|| "crypto".to_string());
            commit_key_to_tree(&r, &target, &name, is_private, &patterns, &filter_name)?;
        }
        Ok(())
    }

    /// Read the stored OpenSSH public key at `refs/keys/<name>.pub` (default `sign`).
    #[napi]
    pub fn get_public_key(&self, name: Option<String>) -> Result<String> {
        let r = thread_local(&self.repo);
        let name = name.unwrap_or_else(|| "sign".to_string());
        let bytes = read_ref_blob(&r, &format!("refs/keys/{name}.pub"))?.ok_or_else(|| {
            napi::Error::from_reason(format!("no public key at refs/keys/{name}.pub"))
        })?;
        String::from_utf8(bytes).map_err(|_| {
            napi::Error::from_reason("stored public key is not valid UTF-8".to_string())
        })
    }

    /// Ed25519-sign `data` with the stored seed at `refs/keys/<name>.pem` (default `sign`). The
    /// seed is decrypted with the repo's symmetric crypto key.
    #[napi]
    pub fn sign(&self, data: Buffer, name: Option<String>) -> Result<Buffer> {
        let r = thread_local(&self.repo);
        let name = name.unwrap_or_else(|| "sign".to_string());

        let enc = read_ref_blob(&r, &format!("refs/keys/{name}.pem"))?.ok_or_else(|| {
            napi::Error::from_reason(format!("no signing key at refs/keys/{name}.pem"))
        })?;
        let seed_bytes = self.load_key()?.decrypt(&enc)?;
        let seed: [u8; 32] = seed_bytes.as_slice().try_into().map_err(|_| {
            napi::Error::from_reason("stored signing key is not 32 bytes".to_string())
        })?;
        let sig = sign_with_seed(&seed, data.as_ref())?;
        Ok(Buffer::from(sig))
    }

    /// Verify an Ed25519 `signature` over `data` using the stored public key at
    /// `refs/keys/<name>.pub` (default `sign`).
    #[napi]
    pub fn verify(&self, data: Buffer, signature: Buffer, name: Option<String>) -> Result<bool> {
        let name = name.unwrap_or_else(|| "sign".to_string());
        let pub_text = self.get_public_key(Some(name))?;
        SshIdentity::verify_with_public_key(pub_text, data, signature)
    }

    /// List the distinct key names stored under `refs/keys/*` (e.g. `["sign", "alice"]`).
    #[napi]
    pub fn list_keys(&self) -> Result<Vec<String>> {
        let r = thread_local(&self.repo);
        Ok(list_key_names(&r)?)
    }

    /// Authenticate an OpenSSH/PKCS#8 ed25519 identity against the vault (feature `ssh`).
    ///
    /// The identity's private key is parsed (proving possession of a valid keypair), located by
    /// matching its public key against the stored `refs/keys/*.pub` refs, and then a fixed auth
    /// challenge is signed with the private key and verified against the stored public key — so a
    /// match confirms the registered keys are a genuine, working keypair. Returns `false` when the
    /// key cannot be opened (wrong passphrase / malformed) or is not registered.
    #[napi]
    pub fn authenticate(&self, identity_pem: String, passphrase: Option<String>) -> Result<bool> {
        let r = thread_local(&self.repo);
        let pass = passphrase.unwrap_or_default();

        // Private key → prove possession: sign an auth challenge and verify it against the matching
        // stored public key. This confirms the registered keypair is genuine and that the caller
        // holds the private key.
        if let Ok((seed, identity_pub)) = parse_ed25519_private(identity_pem.trim(), &pass) {
            let Some((_name, stored_pub_text)) = find_stored_public(&r, &identity_pub)? else {
                return Ok(false);
            };
            let challenge: &[u8] = b"gitcrypt-authenticate-v1";
            let sig = sign_with_seed(&seed, challenge)?;
            return SshIdentity::verify_with_public_key(
                stored_pub_text,
                Buffer::from(challenge.to_vec()),
                Buffer::from(sig),
            );
        }

        // Public key only → membership check (possession cannot be proven without the private key).
        if let Ok(identity_pub) = parse_ed25519_public(identity_pem.trim()) {
            return Ok(find_stored_public(&r, &identity_pub)?.is_some());
        }
        Ok(false)
    }
}

// ---------------------------------------------------------------------------
// XMSS^MT vault methods (feature = "xmss")
// ---------------------------------------------------------------------------

#[cfg(feature = "xmss")]
#[napi]
impl Crypto {
    /// Generate an XMSS^MT signing key (XMSSMT-SHA2_20/2_256) and store it in the vault:
    /// `refs/keys/<name>.xmss.vk` (public, plaintext) and `refs/keys/<name>.xmss.sk`
    /// (signing key, encrypted at rest under `SHA256(initial commit) || passphrase`).
    ///
    /// STATEFUL: the key yields a bounded number of signatures; sign with [`Crypto::sign_xmss`].
    #[napi]
    pub fn generate_xmss_key(&self, name: String, passphrase: Option<String>) -> Result<()> {
        let r = thread_local(&self.repo);
        let initial = initial_commit_id(&r)?;
        let pass = passphrase.unwrap_or_default();

        let mut seed = [0u8; XMSS_SEED_LEN];
        SystemRandom::new()
            .fill(&mut seed)
            .map_err(|_| napi::Error::from_reason("RNG failure".to_string()))?;

        let mut kp = KeyPair::<XmssP>::from_seed(&seed)
            .map_err(|e| napi::Error::from_reason(format!("xmss keygen: {e}")))?;
        let sk0 = kp.signing_key().as_ref().to_vec();
        let vk = kp.verifying_key().as_ref().to_vec();

        let enc = vault_seal(&vault_aead_key(&initial, &pass), &sk0)?;
        write_key_ref(&r, &format!("refs/keys/{name}.xmss.sk"), &enc)?;
        write_key_ref(&r, &format!("refs/keys/{name}.xmss.vk"), &vk)?;
        Ok(())
    }

    /// Number of signatures already issued by the XMSS^MT key `name` (observability).
    #[napi]
    pub fn xmss_used(&self, name: String) -> Result<u32> {
        let r = thread_local(&self.repo);
        let used = match read_ref_blob(&r, &format!("refs/keys/{name}.xmss.hwm"))? {
            Some(b) if b.len() == 8 => {
                u64::from_be_bytes(b.as_slice().try_into().unwrap()).saturating_add(1)
            }
            _ => 0,
        };
        Ok(u32::try_from(used).unwrap_or(u32::MAX))
    }

    /// XMSS^MT-sign `data` with the stored key `name`. STATEFUL: advances and persists the
    /// one-time index. Refuses if the key is exhausted, or if a rolled-back / already-used index
    /// is detected via the write-ahead high-water mark.
    #[napi]
    pub fn sign_xmss(
        &self,
        name: String,
        data: Buffer,
        passphrase: Option<String>,
    ) -> Result<Buffer> {
        let r = thread_local(&self.repo);
        let initial = initial_commit_id(&r)?;
        let pass = passphrase.unwrap_or_default();

        let enc = read_ref_blob(&r, &format!("refs/keys/{name}.xmss.sk"))?.ok_or_else(|| {
            napi::Error::from_reason(format!("no xmss signing key at refs/keys/{name}.xmss.sk"))
        })?;
        let sk_bytes = vault_open(&vault_aead_key(&initial, &pass), &enc)?;

        let idx = xmss_read_index(&sk_bytes)?;
        if idx >= XMSS_TOTAL_SIGNATURES {
            return Err(napi::Error::from_reason(
                "xmss signing key exhausted".to_string(),
            ));
        }
        // Rollback / reuse guard: never sign with an index <= the persisted high-water mark.
        if let Some(hwm) = read_ref_blob(&r, &format!("refs/keys/{name}.xmss.hwm"))? {
            let hwm = u64::from_be_bytes(hwm.as_slice().try_into().map_err(|_| {
                napi::Error::from_reason("corrupt xmss high-water mark".to_string())
            })?);
            if idx <= hwm {
                return Err(napi::Error::from_reason(
                    "xmss key state rolled back or already used; refusing to reuse a one-time index"
                        .to_string(),
                ));
            }
        }

        // Reserve this index (write-ahead) so a crash can never cause reuse.
        write_key_ref(
            &r,
            &format!("refs/keys/{name}.xmss.hwm"),
            &idx.to_be_bytes(),
        )?;

        // Sign (advances the in-memory index to idx + 1).
        let mut sk = SigningKey::<XmssP>::try_from(sk_bytes.as_slice())
            .map_err(|e| napi::Error::from_reason(format!("decode xmss sk: {e}")))?;
        let sig = sk
            .sign_detached(data.as_ref())
            .map_err(|e| napi::Error::from_reason(format!("xmss sign: {e}")))?;

        // Persist the advanced signing key.
        let advanced = sk.as_ref().to_vec();
        let enc2 = vault_seal(&vault_aead_key(&initial, &pass), &advanced)?;
        write_key_ref(&r, &format!("refs/keys/{name}.xmss.sk"), &enc2)?;

        Ok(Buffer::from(sig.as_ref().to_vec()))
    }

    /// Verify an XMSS^MT `signature` over `data` against the stored public key
    /// `refs/keys/<name>.xmss.vk`.
    #[napi]
    pub fn verify_xmss(&self, name: String, data: Buffer, signature: Buffer) -> Result<bool> {
        let r = thread_local(&self.repo);
        let vk_bytes =
            read_ref_blob(&r, &format!("refs/keys/{name}.xmss.vk"))?.ok_or_else(|| {
                napi::Error::from_reason(format!("no xmss public key at refs/keys/{name}.xmss.vk"))
            })?;
        let vk = VerifyingKey::<XmssP>::try_from(vk_bytes.as_slice())
            .map_err(|e| napi::Error::from_reason(format!("decode xmss vk: {e}")))?;
        let sig = DetachedSignature::<XmssP>::try_from(signature.as_ref())
            .map_err(|e| napi::Error::from_reason(format!("decode xmss signature: {e}")))?;
        Ok(vk.verify_detached(&sig, data.as_ref()).is_ok())
    }
}

#[cfg(feature = "gpg")]
#[napi]
impl Crypto {
    /// Add a GPG recipient (feature `gpg`). Keyring lookup is not yet implemented, so this
    /// returns an actionable error rather than panicking. The repository is opened
    /// automatically from the current directory.
    #[napi]
    pub fn add_gpg_user(gpg_id: String) -> Result<()> {
        let git_dir = open_repo_git_dir(".")?;
        let km = KeyManager::new(&git_dir);
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized.into());
        }
        let key = km.load_key()?;
        let encrypted_key = GpgManager::encrypt_key_for_recipient(&key, &gpg_id)?;
        let gpg_keys_dir = km.git_crypt_dir().join("keys").join("gpg");
        std::fs::create_dir_all(&gpg_keys_dir)?;
        std::fs::write(gpg_keys_dir.join(format!("{gpg_id}.key")), encrypted_key)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Internal git filter drivers (stdin/stdout) — for a future CLI binary, not NAPI.
// ---------------------------------------------------------------------------

/// Clean filter: encrypt stdin to stdout (idempotent on already-encrypted input).
#[allow(dead_code)]
pub fn clean_filter(key: &SymmetricCryptoKey) -> CryptoResult<()> {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;
    if SymmetricCryptoKey::is_encrypted(&input) {
        io::stdout().write_all(&input)?;
        return Ok(());
    }
    let encrypted = key.encrypt(&input)?;
    io::stdout().write_all(&encrypted)?;
    Ok(())
}

/// Smudge filter: decrypt stdin to stdout (pass-through if not encrypted).
#[allow(dead_code)]
pub fn smudge_filter(key: &SymmetricCryptoKey) -> CryptoResult<()> {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;
    if !SymmetricCryptoKey::is_encrypted(&input) {
        io::stdout().write_all(&input)?;
        return Ok(());
    }
    let decrypted = key.decrypt(&input)?;
    io::stdout().write_all(&decrypted)?;
    Ok(())
}

/// Diff filter: replace encrypted content with a placeholder notice.
#[allow(dead_code)]
pub fn diff_filter() -> CryptoResult<()> {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;
    if SymmetricCryptoKey::is_encrypted(&input) {
        writeln!(io::stdout(), "*** This file is encrypted with crypt ***")?;
    } else {
        io::stdout().write_all(&input)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// SSH / age recipient support (feature = "ssh")
// ---------------------------------------------------------------------------

/// Derive a filesystem-safe recipient name from an SSH public key, preferring the comment
/// field, then the file stem, then a truncated SHA-256 fingerprint.
#[cfg(feature = "ssh")]
fn derive_recipient_name(ssh_key: &str, ssh_key_path: &Path) -> String {
    ssh_key
        .split_whitespace()
        .nth(2)
        .map(sanitize_label)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            ssh_key_path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(sanitize_label)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| fallback_fingerprint(ssh_key))
}

/// Keep only filesystem-safe characters.
#[cfg(feature = "ssh")]
fn sanitize_label(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect()
}

/// Deterministic `ssh-<hex>` fallback name.
#[cfg(feature = "ssh")]
fn fallback_fingerprint(data: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    let digest = hasher.finalize();
    format!("ssh-{}", hex::encode(&digest[..8]))
}

#[cfg(feature = "ssh")]
pub use crate::vault::IdentityManager;

// ---------------------------------------------------------------------------
// SSH key parsing / conversion helpers (feature = "ssh")
// ---------------------------------------------------------------------------

/// Parse an `ssh-ed25519` public key (or a private key, extracting its public) into the
/// 32-byte Edwards-point encoding.
#[cfg(feature = "ssh")]
fn ssh_ed25519_public_bytes(recipient: &str) -> CryptoResult<[u8; 32]> {
    if let Ok(pubkey) = ssh_key::PublicKey::from_openssh(recipient.trim()) {
        if let Ok(bytes) = ed25519_pub_from_ssh_public(&pubkey) {
            return Ok(bytes);
        }
    }
    let privkey = ssh_key::PrivateKey::from_openssh(recipient.trim())
        .map_err(|e| CryptoError::Ssh(format!("invalid SSH key: {e}")))?;
    ed25519_pub_from_ssh_private(&privkey)
}

#[cfg(feature = "ssh")]
fn ed25519_pub_from_ssh_public(pubkey: &ssh_key::PublicKey) -> CryptoResult<[u8; 32]> {
    use ssh_key::public::KeyData;
    match pubkey.key_data() {
        KeyData::Ed25519(p) => {
            let b: &[u8] = p.as_ref();
            if b.len() != 32 {
                return Err(CryptoError::Ssh("bad ed25519 public key".into()));
            }
            let mut a = [0u8; 32];
            a.copy_from_slice(b);
            Ok(a)
        }
        _ => Err(CryptoError::Ssh(
            "only ed25519 SSH keys are supported".into(),
        )),
    }
}

#[cfg(feature = "ssh")]
fn ed25519_pub_from_ssh_private(privkey: &ssh_key::PrivateKey) -> CryptoResult<[u8; 32]> {
    use ssh_key::private::KeypairData;
    match privkey.key_data() {
        KeypairData::Ed25519(kp) => {
            let b: &[u8] = kp.public.as_ref();
            if b.len() != 32 {
                return Err(CryptoError::Ssh("bad ed25519 public key".into()));
            }
            let mut a = [0u8; 32];
            a.copy_from_slice(b);
            Ok(a)
        }
        _ => Err(CryptoError::Ssh(
            "only ed25519 SSH keys are supported".into(),
        )),
    }
}

#[cfg(feature = "ssh")]
fn ssh_ed25519_seed(identity_content: &str, passphrase: Option<&str>) -> CryptoResult<[u8; 32]> {
    use ssh_key::private::KeypairData;
    let privkey = if let Some(pw) = passphrase {
        ssh_key::PrivateKey::from_openssh(identity_content.trim())
            .and_then(|k| k.decrypt(pw))
            .map_err(|e| CryptoError::Ssh(format!("failed to decrypt SSH identity: {e}")))
    } else {
        ssh_key::PrivateKey::from_openssh(identity_content.trim()).map_err(|e| {
            CryptoError::Ssh(format!(
                "invalid/unencrypted SSH identity (passphrase needed?): {e}"
            ))
        })
    }?;
    match privkey.key_data() {
        KeypairData::Ed25519(kp) => Ok(kp.private.to_bytes()),
        _ => Err(CryptoError::Ssh(
            "only ed25519 SSH keys are supported".into(),
        )),
    }
}

/// Convert an ed25519 public point into an X25519 public key (Edwards -> Montgomery).
#[cfg(feature = "ssh")]
fn ed25519_to_x25519_public(edwards: &[u8; 32]) -> CryptoResult<X25519PublicKey> {
    use curve25519_dalek::edwards::CompressedEdwardsY;
    use curve25519_dalek::montgomery::MontgomeryPoint;
    let compressed = CompressedEdwardsY::from_slice(edwards)
        .map_err(|_| CryptoError::Ssh("invalid ed25519 public point".into()))?;
    let point = compressed
        .decompress()
        .ok_or_else(|| CryptoError::Ssh("invalid ed25519 public point".into()))?;
    let mont: MontgomeryPoint = point.to_montgomery();
    Ok(X25519PublicKey::from(mont.to_bytes()))
}

/// Derive the X25519 scalar from an ed25519 seed (SHA-512, then clamp).
#[cfg(feature = "ssh")]
fn ed25519_seed_to_x25519_scalar(seed: &[u8; 32]) -> [u8; 32] {
    use sha2::{Digest, Sha512};
    let hash = Sha512::digest(seed);
    let mut scalar = [0u8; 32];
    scalar.copy_from_slice(&hash[..32]);
    // Ed25519/X25519 clamp (idempotent — dalek re-clamps on use).
    scalar[0] &= 248;
    scalar[31] &= 127;
    scalar[31] |= 64;
    scalar
}

/// Derive the AEAD key from an X25519 shared secret via HKDF-SHA256.
#[cfg(feature = "ssh")]
fn derive_aead_key(shared: &[u8]) -> CryptoResult<[u8; 32]> {
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, b"gitcrypt-ssh-v1");
    let prk = salt.extract(shared);
    let okm = prk
        .expand(&[b"ssh-recipient-key"], hkdf::HKDF_SHA256)
        .map_err(|_| CryptoError::Ssh("hkdf expand failed".into()))?;
    let mut key = [0u8; 32];
    okm.fill(&mut key)
        .map_err(|_| CryptoError::Ssh("hkdf fill failed".into()))?;
    Ok(key)
}

#[cfg(feature = "ssh")]
fn aead_seal(key: &[u8; 32], nonce: &[u8; NONCE_SIZE], plaintext: &[u8]) -> CryptoResult<Vec<u8>> {
    let unbound = UnboundKey::new(&AES_256_GCM, key)
        .map_err(|_| CryptoError::Ssh("invalid aead key".into()))?;
    let key = LessSafeKey::new(unbound);
    let nonce = Nonce::try_assume_unique_for_key(nonce)
        .map_err(|_| CryptoError::Ssh("invalid nonce".into()))?;
    let mut in_out = plaintext.to_vec();
    key.seal_in_place_append_tag(nonce, Aad::empty(), &mut in_out)
        .map_err(|_| CryptoError::Ssh("encryption failed".into()))?;
    Ok(in_out)
}

#[cfg(feature = "ssh")]
fn aead_open(key: &[u8; 32], nonce: &[u8; NONCE_SIZE], ciphertext: &[u8]) -> CryptoResult<Vec<u8>> {
    let unbound = UnboundKey::new(&AES_256_GCM, key)
        .map_err(|_| CryptoError::Ssh("invalid aead key".into()))?;
    let key = LessSafeKey::new(unbound);
    let nonce = Nonce::try_assume_unique_for_key(nonce)
        .map_err(|_| CryptoError::Ssh("invalid nonce".into()))?;
    let mut in_out = ciphertext.to_vec();
    let plain = key
        .open_in_place(nonce, Aad::empty(), &mut in_out)
        .map_err(|_| CryptoError::Ssh("decryption failed (wrong key or tampered)".into()))?;
    Ok(plain.to_vec())
}

// ---------------------------------------------------------------------------
// Ref-backed key vault (feature = "ssh")
//
// Keys live in git refs under `refs/keys/`:
//   - `refs/keys/<name>.pub`  public key, plaintext (OpenSSH text)
//   - `refs/keys/<name>.pem`  private key, encrypted at rest with AES-256-GCM under
//                             `SHA256(SHA256(initial commit) || passphrase)`
// The default signing key is `refs/keys/sign`. The "initial commit" is the commit pointed to by
// `refs/heads/meta/repository`.
// ---------------------------------------------------------------------------

/// Magic header for the at-rest encrypted private-key envelope.
#[cfg(feature = "ssh")]
const VAULT_MAGIC: &[u8; 8] = b"KEYVAULT";

/// Resolve the repository's "initial commit".
///
/// If an explicit pin `refs/heads/meta/repository` exists it is used — so the at-rest key stays
/// stable even if history is later rewritten. Otherwise the commit is derived natively (no manual
/// tagging needed) by walking HEAD's first-parent chain to the root: the commit with no parents.
#[cfg(feature = "ssh")]
fn initial_commit_id(r: &gix::Repository) -> CryptoResult<gix::ObjectId> {
    // 1. Explicit pin, if present.
    if let Ok(mut reference) = r.find_reference("refs/heads/meta/repository") {
        if let Ok(id) = reference.peel_to_id() {
            return Ok(id.detach());
        }
    }
    // 2. Derive: walk first-parent from HEAD down to the root commit.
    let head = r
        .head_commit()
        .map_err(|e| CryptoError::Git(format!("head_commit (repo has no commits?): {e}")))?;
    let mut current = head.id().detach();
    loop {
        let commit = r
            .find_commit(current)
            .map_err(|e| CryptoError::Git(format!("find_commit: {e}")))?;
        match commit.parent_ids().next() {
            Some(parent) => current = parent.detach(),
            None => return Ok(current),
        }
    }
}

/// Derive the AES-256 key protecting a stored private key: `SHA256(SHA256(initial) || passphrase)`.
#[cfg(feature = "ssh")]
fn vault_aead_key(initial: &gix::ObjectId, passphrase: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut material = Sha256::digest(initial.as_bytes()).to_vec();
    material.extend_from_slice(passphrase.as_bytes());
    let digest = Sha256::digest(&material);
    let mut key = [0u8; 32];
    key.copy_from_slice(&digest);
    key
}

/// Encrypt a stored private key: `VAULT_MAGIC || nonce || ciphertext+tag`.
#[cfg(feature = "ssh")]
fn vault_seal(key: &[u8; 32], plaintext: &[u8]) -> CryptoResult<Vec<u8>> {
    let mut nonce = [0u8; NONCE_SIZE];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| CryptoError::Ssh("RNG failure".into()))?;
    let ct = aead_seal(key, &nonce, plaintext)?;
    let mut out = Vec::with_capacity(VAULT_MAGIC.len() + NONCE_SIZE + ct.len());
    out.extend_from_slice(VAULT_MAGIC);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Decrypt a stored private-key envelope produced by [`vault_seal`].
#[cfg(feature = "ssh")]
fn vault_open(key: &[u8; 32], blob: &[u8]) -> CryptoResult<Vec<u8>> {
    let min = VAULT_MAGIC.len() + NONCE_SIZE;
    if blob.len() < min || &blob[..VAULT_MAGIC.len()] != VAULT_MAGIC {
        return Err(CryptoError::Ssh("invalid key envelope".into()));
    }
    let (_, rest) = blob.split_at(VAULT_MAGIC.len());
    let (nonce_bytes, ct) = rest.split_at(NONCE_SIZE);
    let mut nonce = [0u8; NONCE_SIZE];
    nonce.copy_from_slice(nonce_bytes);
    aead_open(key, &nonce, ct)
}

/// Write `bytes` as a blob and point the ref `full_name` at it (creating or updating).
#[cfg(feature = "ssh")]
fn write_key_ref(r: &gix::Repository, full_name: &str, bytes: &[u8]) -> CryptoResult<()> {
    let oid = r
        .write_blob(bytes.to_vec())
        .map_err(|e| CryptoError::Git(format!("write_blob: {e}")))?;
    r.reference(
        full_name,
        oid,
        gix::refs::transaction::PreviousValue::Any,
        "crypt: store key",
    )
    .map_err(|e| CryptoError::Git(format!("create ref {full_name}: {e}")))?;
    Ok(())
}

/// Read the blob referenced by `full_name`, or `None` if the ref does not exist.
#[cfg(feature = "ssh")]
fn read_ref_blob(r: &gix::Repository, full_name: &str) -> CryptoResult<Option<Vec<u8>>> {
    let reference = match r.try_find_reference(full_name) {
        Ok(Some(rf)) => rf,
        Ok(None) => return Ok(None),
        Err(e) => return Err(CryptoError::Git(format!("find_reference: {e}"))),
    };
    let id = match reference.try_id() {
        Some(id) => id.detach(),
        None => return Err(CryptoError::Git(format!("{full_name} is a symbolic ref"))),
    };
    let blob = r
        .find_blob(id)
        .map_err(|e| CryptoError::Git(format!("find_blob: {e}")))?;
    Ok(Some(blob.data.to_vec()))
}

/// List the distinct key names stored under `refs/keys/*` (stripping `.pub`/`.pem`).
#[cfg(feature = "ssh")]
fn list_key_names(r: &gix::Repository) -> CryptoResult<Vec<String>> {
    use std::collections::BTreeSet;
    let platform = r
        .references()
        .map_err(|e| CryptoError::Git(format!("references: {e}")))?;
    let iter = platform
        .prefixed(b"refs/keys/")
        .map_err(|e| CryptoError::Git(format!("prefixed: {e}")))?;
    let mut names = BTreeSet::new();
    for item in iter {
        let reference = item.map_err(|e| CryptoError::Git(format!("iterate refs: {e}")))?;
        let full = reference.name().as_bstr().to_string();
        if let Some(rest) = full.strip_prefix("refs/keys/") {
            let name = rest
                .strip_suffix(".pub")
                .or_else(|| rest.strip_suffix(".pem"))
                .unwrap_or(rest);
            names.insert(name.to_string());
        }
    }
    Ok(names.into_iter().collect())
}

/// Parse an ed25519 private key from any supported encoding — OpenSSH
/// (`-----BEGIN OPENSSH PRIVATE KEY-----`) or PKCS#8 (`-----BEGIN PRIVATE KEY-----` /
/// `-----BEGIN ENCRYPTED PRIVATE KEY-----`) — returning the 32-byte seed and 32-byte public key.
/// `passphrase` decrypts passphrase-protected keys.
#[cfg(feature = "ssh")]
fn parse_ed25519_private(pem: &str, passphrase: &str) -> CryptoResult<([u8; 32], [u8; 32])> {
    use ssh_key::private::KeypairData;

    // 1. OpenSSH private key.
    if let Ok(privkey) = ssh_key::PrivateKey::from_openssh(pem.trim()) {
        let privkey = if privkey.is_encrypted() {
            if passphrase.is_empty() {
                return Err(CryptoError::Ssh(
                    "private key is passphrase-protected".into(),
                ));
            }
            privkey
                .decrypt(passphrase)
                .map_err(|e| CryptoError::Ssh(format!("decrypt SSH key: {e}")))?
        } else {
            privkey
        };
        return match privkey.key_data() {
            KeypairData::Ed25519(kp) => {
                let seed = kp.private.to_bytes();
                let pub_bytes: &[u8] = kp.public.as_ref();
                let mut public = [0u8; 32];
                public.copy_from_slice(pub_bytes);
                Ok((seed, public))
            }
            _ => Err(CryptoError::Ssh("only ed25519 keys are supported".into())),
        };
    }

    // 2. PKCS#8 private key (unencrypted). Encrypted PKCS#8 must be decrypted by the caller first.
    let kp = ed25519::pkcs8::KeypairBytes::from_pkcs8_pem(pem.trim()).map_err(|e| {
        CryptoError::Ssh(format!(
            "unrecognized ed25519 private key (OpenSSH or unencrypted PKCS#8 PEM): {e}"
        ))
    })?;
    let seed = kp.secret_key;
    let public: [u8; 32] = match kp.public_key {
        Some(pk) => pk.to_bytes(),
        None => derive_ed25519_public(&seed)?,
    };
    Ok((seed, public))
}

/// Parse an ed25519 public key from any supported encoding — OpenSSH one-line
/// (`ssh-ed25519 AAAA...`) or PKCS#8 SubjectPublicKeyInfo PEM (`-----BEGIN PUBLIC KEY-----`).
#[cfg(feature = "ssh")]
fn parse_ed25519_public(pem: &str) -> CryptoResult<[u8; 32]> {
    // 1. OpenSSH public key.
    if let Ok(pk) = ssh_key::PublicKey::from_openssh(pem.trim()) {
        return ed25519_pub_from_ssh_public(&pk);
    }
    // 2. PKCS#8 SubjectPublicKeyInfo PEM.
    let pk = ed25519::pkcs8::PublicKeyBytes::from_public_key_pem(pem.trim())
        .map_err(|e| CryptoError::Ssh(format!("unrecognized ed25519 public key: {e}")))?;
    Ok(pk.to_bytes())
}

/// Build the OpenSSH text form (`ssh-ed25519 AAAA...`) for a raw 32-byte ed25519 public key.
#[cfg(feature = "ssh")]
fn ed25519_public_openssh_text(public: &[u8; 32]) -> CryptoResult<String> {
    let mut wire = Vec::with_capacity(4 + 11 + 4 + 32);
    wire.extend_from_slice(&(11u32).to_be_bytes());
    wire.extend_from_slice(b"ssh-ed25519");
    wire.extend_from_slice(&(32u32).to_be_bytes());
    wire.extend_from_slice(public);
    let pk = ssh_key::PublicKey::from_bytes(&wire)
        .map_err(|e| CryptoError::Ssh(format!("build public key: {e}")))?;
    pk.to_openssh()
        .map_err(|e| CryptoError::Ssh(format!("serialize public key: {e}")))
}

/// Derive the ed25519 public key from a 32-byte seed.
#[cfg(feature = "ssh")]
fn derive_ed25519_public(seed: &[u8; 32]) -> CryptoResult<[u8; 32]> {
    use ring::signature::KeyPair as _;
    let kp = signature::Ed25519KeyPair::from_seed_unchecked(seed)
        .map_err(|_| CryptoError::Ssh("invalid ed25519 seed".into()))?;
    let mut public = [0u8; 32];
    public.copy_from_slice(kp.public_key().as_ref());
    Ok(public)
}

/// Ed25519-sign `data` with a raw 32-byte seed, returning the 64-byte signature.
#[cfg(feature = "ssh")]
fn sign_with_seed(seed: &[u8; 32], data: &[u8]) -> CryptoResult<Vec<u8>> {
    let kp = signature::Ed25519KeyPair::from_seed_unchecked(seed)
        .map_err(|_| CryptoError::Ssh("invalid ed25519 seed".into()))?;
    Ok(kp.sign(data).as_ref().to_vec())
}

/// Find the stored `refs/keys/*.pub` whose public key matches `identity_pub`, returning its
/// name and OpenSSH text.
#[cfg(feature = "ssh")]
fn find_stored_public(
    r: &gix::Repository,
    identity_pub: &[u8; 32],
) -> CryptoResult<Option<(String, String)>> {
    let platform = r
        .references()
        .map_err(|e| CryptoError::Git(format!("references: {e}")))?;
    let iter = platform
        .prefixed(b"refs/keys/")
        .map_err(|e| CryptoError::Git(format!("prefixed: {e}")))?;
    for item in iter {
        let reference = item.map_err(|e| CryptoError::Git(format!("iterate refs: {e}")))?;
        let full = reference.name().as_bstr().to_string();
        let Some(name) = full
            .strip_prefix("refs/keys/")
            .and_then(|s| s.strip_suffix(".pub"))
        else {
            continue;
        };
        let Some(pub_bytes) = read_ref_blob(r, &full)? else {
            continue;
        };
        let pub_text = String::from_utf8(pub_bytes).unwrap_or_default();
        let Ok(stored_pub) = parse_ed25519_public(&pub_text) else {
            continue;
        };
        if &stored_pub == identity_pub {
            return Ok(Some((name.to_string(), pub_text)));
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// XMSS^MT stateful signatures (feature = "xmss")
//
// Fixed parameter set XMSSMT-SHA2_20/2_256 (n=32, total height 40 → 2^40 signatures).
// STATEFUL: each signature consumes one one-time leaf. The signing key (with its advanced
// index) is persisted after every sign, and a write-ahead high-water mark guards against index
// reuse (crash/rollback). Do not run two signs concurrently on the same key.
// ---------------------------------------------------------------------------

/// Total signatures available for XMSSMT-SHA2_20/2_256 (2^40).
#[cfg(feature = "xmss")]
const XMSS_TOTAL_SIGNATURES: u64 = 1u64 << 40;
/// Bytes of the big-endian index embedded in the compact signing key (ceil(40 / 8)).
#[cfg(feature = "xmss")]
const XMSS_INDEX_BYTES: usize = 5;
/// Seed length for `KeyPair::from_seed` (3 * n, n = 32).
#[cfg(feature = "xmss")]
const XMSS_SEED_LEN: usize = 96;

/// Read the big-endian one-time index from an RFC 8391 compact signing key
/// (`OID(4) || index(idx_bytes) || …`).
#[cfg(feature = "xmss")]
fn xmss_read_index(sk: &[u8]) -> CryptoResult<u64> {
    if sk.len() < 4 + XMSS_INDEX_BYTES {
        return Err(CryptoError::Xmss("signing key too short".into()));
    }
    let mut idx = 0u64;
    for &b in &sk[4..4 + XMSS_INDEX_BYTES] {
        idx = (idx << 8) | u64::from(b);
    }
    Ok(idx)
}

/// Options for storing a key via [`Crypto::add_key`].
#[cfg(feature = "ssh")]
#[napi(object)]
pub struct KeyStoreOptions {
    /// When set, also commit the key material as a new commit. A full ref
    /// (`refs/heads/meta/repository`) or short name (`meta/repository`) extends that branch's tip;
    /// the sentinel `"/"` creates an *orphan* (parentless) commit on `refs/heads/<name>`.
    /// Default (`undefined`): only write refs under `refs/keys/*`, no commit.
    pub commit_to_tree: Option<String>,
    /// `.gitattributes` patterns to write (each gets `filter=<filterName> diff=<filterName>`
    /// appended). Defaults to a small sensible set.
    pub gitattributes: Option<Vec<String>>,
    /// The git filter name applied to the committed `.gitattributes` patterns. Must match the
    /// name configured by [`Crypto::init`]; defaults to `crypto`.
    pub filter_name: Option<String>,
}

/// Default `.gitattributes` patterns written when committing key material: encrypt `.pem` only.
#[cfg(feature = "ssh")]
fn default_gitattributes_patterns() -> Vec<String> {
    vec!["**.pem".to_string()]
}

/// Commit the stored key material (`.gitattributes`, `.<name>.pub`, and — for private keys — the
/// encrypted `.<name>.pem`) onto the branch named by `commit_to_tree`, as a new commit whose parent
/// is the branch's current tip. The committer is fixed to `repo@chen.software`.
#[cfg(feature = "ssh")]
fn commit_key_to_tree(
    r: &gix::Repository,
    commit_to_tree: &str,
    name: &str,
    include_private: bool,
    patterns: &[String],
    filter_name: &str,
) -> CryptoResult<()> {
    // `"/"` is a sentinel for an orphan (parentless) commit on a per-key branch
    // `refs/heads/<name>`. Otherwise treat the value as a branch ref (full or short).
    let orphan = commit_to_tree == "/";
    let full_ref = if orphan {
        format!("refs/heads/{name}")
    } else if commit_to_tree.starts_with("refs/") {
        commit_to_tree.to_string()
    } else {
        format!("refs/heads/{commit_to_tree}")
    };

    // Base tree + parent: an orphan starts from the empty tree with no parent; otherwise it
    // extends the branch's current tip commit.
    let (parent, base_tree) = if orphan {
        (None, gix::ObjectId::empty_tree(r.object_hash()))
    } else {
        let mut reference = r
            .find_reference(full_ref.as_str())
            .map_err(|e| CryptoError::Git(format!("find {full_ref}: {e}")))?;
        let tip = reference
            .peel_to_id()
            .map_err(|e| CryptoError::Git(format!("peel {full_ref}: {e}")))?
            .detach();
        let tree = r
            .find_commit(tip)
            .map_err(|e| CryptoError::Git(format!("find_commit: {e}")))?
            .tree_id()
            .map_err(|e| CryptoError::Git(format!("tree_id: {e}")))?
            .detach();
        (Some(tip), tree)
    };

    let mut editor = r
        .edit_tree(base_tree)
        .map_err(|e| CryptoError::Git(format!("edit_tree: {e}")))?;

    // .gitattributes declaring the encrypted file patterns.
    let mut attrs = String::new();
    for p in patterns {
        attrs.push_str(p);
        attrs.push_str(&format!(" filter={filter_name} diff={filter_name}\n"));
    }
    let attrs_blob = r
        .write_blob(attrs.into_bytes())
        .map_err(|e| CryptoError::Git(format!("write_blob: {e}")))?;
    editor
        .upsert(
            ".gitattributes",
            gix::objs::tree::EntryKind::Blob,
            attrs_blob,
        )
        .map_err(|e| CryptoError::Git(format!("upsert .gitattributes: {e}")))?;

    // .<name>.pub (public key, plaintext).
    if let Some(pub_bytes) = read_ref_blob(r, &format!("refs/keys/{name}.pub"))? {
        let blob = r
            .write_blob(pub_bytes)
            .map_err(|e| CryptoError::Git(format!("write_blob: {e}")))?;
        editor
            .upsert(
                format!(".{name}.pub"),
                gix::objs::tree::EntryKind::Blob,
                blob,
            )
            .map_err(|e| CryptoError::Git(format!("upsert .{name}.pub: {e}")))?;
    }

    // .<name>.pem (encrypted-at-rest private key).
    if include_private {
        if let Some(pem_bytes) = read_ref_blob(r, &format!("refs/keys/{name}.pem"))? {
            let blob = r
                .write_blob(pem_bytes)
                .map_err(|e| CryptoError::Git(format!("write_blob: {e}")))?;
            editor
                .upsert(
                    format!(".{name}.pem"),
                    gix::objs::tree::EntryKind::Blob,
                    blob,
                )
                .map_err(|e| CryptoError::Git(format!("upsert .{name}.pem: {e}")))?;
        }
    }

    let new_tree = editor
        .write()
        .map_err(|e| CryptoError::Git(format!("write tree: {e}")))?
        .detach();

    // Fixed committer identity (repo@chen.software), UTC timestamp.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let time = format!("{secs} +0000");
    let sig: gix::actor::Signature = gix::actor::SignatureRef {
        name: gix::bstr::BStr::new("Repository"),
        email: gix::bstr::BStr::new("repo@chen.software"),
        time: time.as_str(),
    }
    .into();
    let message = format!("Add {name} signing key to vault");
    let commit = gix::objs::Commit {
        tree: new_tree,
        parents: parent.into_iter().collect::<Vec<_>>().into(),
        author: sig.clone(),
        committer: sig,
        encoding: None,
        message: message.into(),
        extra_headers: Vec::new(),
    };
    let commit_id = r
        .write_object(&commit)
        .map_err(|e| CryptoError::Git(format!("write_commit: {e}")))?;
    r.reference(
        full_ref.as_str(),
        commit_id,
        gix::refs::transaction::PreviousValue::Any,
        "crypt: commit key",
    )
    .map_err(|e| CryptoError::Git(format!("update ref {full_ref}: {e}")))?;
    Ok(())
}

/// An ed25519 SSH identity, exposed to JS for asymmetric `sign`/`verify`.
///
/// NOTE: this is separate from [`SymmetricCryptoKey`], which is *symmetric* (only
/// `encrypt`/`decrypt`). Signing/verification is inherently asymmetric, so it lives on the
/// imported SSH keypair, not on the symmetric repo key.
#[cfg(feature = "ssh")]
#[napi]
pub struct SshIdentity {
    seed: [u8; 32],
    public: [u8; 32],
}

#[cfg(feature = "ssh")]
#[napi]
impl SshIdentity {
    /// Load an OpenSSH or PKCS#8 ed25519 identity from PEM text (unencrypted, or
    /// passphrase-protected when `passphrase` is given).
    #[napi(constructor)]
    pub fn from_private_key(pem: String, passphrase: Option<String>) -> Result<Self> {
        let pass = passphrase.unwrap_or_default();
        let (seed, public) = parse_ed25519_private(&pem, &pass)
            .map_err(|e| napi::Error::from_reason(format!("invalid SSH identity: {e}")))?;
        Ok(Self { seed, public })
    }

    /// Ed25519-sign `data`, returning the 64-byte raw signature.
    #[napi]
    pub fn sign(&self, data: Buffer) -> Result<Buffer> {
        let kp = signature::Ed25519KeyPair::from_seed_unchecked(&self.seed)
            .map_err(|_| napi::Error::from_reason("invalid ed25519 seed".to_string()))?;
        let sig = kp.sign(data.as_ref());
        Ok(Buffer::from(sig.as_ref().to_vec()))
    }

    /// Verify an Ed25519 `signature` over `data` using this identity's public key.
    #[napi]
    pub fn verify(&self, data: Buffer, signature: Buffer) -> Result<bool> {
        let pk = signature::UnparsedPublicKey::new(&signature::ED25519, &self.public);
        Ok(pk.verify(data.as_ref(), signature.as_ref()).is_ok())
    }

    /// Verify an Ed25519 `signature` over `data` using a public-key PEM/string.
    #[napi]
    pub fn verify_with_public_key(
        pub_pem: String,
        data: Buffer,
        signature: Buffer,
    ) -> Result<bool> {
        let pubkey = ssh_key::PublicKey::from_openssh(pub_pem.trim())
            .map_err(|e| napi::Error::from_reason(format!("invalid SSH public key: {e}")))?;
        let pub_bytes = ed25519_pub_from_ssh_public(&pubkey)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        let pk = signature::UnparsedPublicKey::new(&signature::ED25519, &pub_bytes);
        Ok(pk.verify(data.as_ref(), signature.as_ref()).is_ok())
    }
}

// ---------------------------------------------------------------------------
// GPG recipient support (feature = "gpg") — keyring lookup is a stub.
// ---------------------------------------------------------------------------

/// rpgp-backed GPG recipient helpers. Keyring lookup is not yet implemented.
#[cfg(feature = "gpg")]
pub struct GpgManager;

#[cfg(feature = "gpg")]
impl GpgManager {
    /// Encrypt a key for a GPG recipient. Currently a stub: certificate/keyring lookup is
    /// not implemented, so this returns an actionable error instead of panicking.
    pub fn encrypt_key_for_recipient(_key: &SymmetricCryptoKey, _recipient: &str) -> CryptoResult<Vec<u8>> {
        Err(CryptoError::Gpg(
            "GPG recipient encryption is not yet implemented (keyring lookup pending)".into(),
        ))
    }
}

// `CryptoKey` is a *stateful* class rather than a plain data object: it holds
// either raw symmetric key material or an OpenSSL key handle, and it backs the
// `new CryptoKey.*` operations. Node's Web Crypto `CryptoKey` is the reference
// API, so the read-only `algorithm` / `extractable` / `type` / `usages`
// accessors and the operation names match it exactly.
//
// Asymmetric generation returns a `CryptoKey` whose `publicKey` / `privateKey`
// getters are populated, so `const { publicKey, privateKey } = await
// new CryptoKey(...)` destructures the same way it does against Node.
//
// Operations are exposed both statically (`CryptoKey.encrypt(alg, key, data)`,
// as documented for Node) and as instance methods (`cryptoKey.encrypt(alg, key,
// data)`), so both call styles work.
//
// Backend: blsful 0.10 — the same provider Node itself uses.

use blsful::bn::{BigNum, BigNumContext};
use blsful::ec::{EcGroup, EcGroupRef, EcKey, EcPoint, PointConversionForm};
use blsful::encrypt::{Decrypter, Encrypter};
use blsful::hash::{MessageDigest, hash};
use blsful::nid::Nid;
use blsful::pkey::{Id, PKey, Private, Public};
use blsful::pkey_ctx::PkeyCtx;
use blsful::rsa::{Padding, Rsa};
use blsful::sign::{RsaPssSaltlen, Signer, Verifier};
use blsful::symm::{Cipher, Crypter, Mode};
use napi::bindgen_prelude::{ClassInstance, Uint8Array};

fn bad(msg: impl Into<String>) -> napi::Error {
    napi::Error::new(napi::Status::InvalidArg, msg.into())
}

fn fail(msg: impl Into<String>) -> napi::Error {
    napi::Error::new(napi::Status::GenericFailure, msg.into())
}

/// Base algorithm object. Always present; carries at least `name`.
#[napi(object)]
#[derive(Clone)]
pub struct KeyAlgorithm {
    /// The algorithm name, e.g. `"SHA-256"`.
    pub name: String,
}

/// The normalized `algorithm` dictionary of a [`CryptoKey`].
#[napi(object)]
#[derive(Clone)]
pub struct CryptoKeyAlgorithm {
    /// The algorithm name, e.g. `"AES-GCM"`, `"RSASSA-PKCS1-v1_5"`, `"ECDSA"`.
    pub name: String,
    /// Symmetric key length in bits.
    pub length: Option<u32>,
    /// Named curve for `ECDSA` / `ECDH`, e.g. `"P-256"`.
    pub named_curve: Option<String>,
    /// Associated hash, e.g. `{ name: "SHA-256" }`.
    pub hash: Option<KeyAlgorithm>,
    /// RSA modulus length in bits.
    pub modulus_length: Option<u32>,
    /// RSA public exponent as bytes (65537 -> `010001`).
    pub public_exponent: Option<Uint8Array>,
}

/// Parameters of a crypto operation (`encrypt`, `decrypt`, `sign`, `verify`,
/// `deriveBits`, `deriveKey`, `wrapKey`, `digest`).
#[napi(object)]
pub struct CryptoOpParams {
    /// Algorithm name, e.g. `"AES-CBC"`.
    pub name: Option<String>,
    /// Initialization vector / nonce / counter block.
    pub iv: Option<Uint8Array>,
    /// Additional authenticated data for AEAD modes.
    pub additional_data: Option<Uint8Array>,
    /// AEAD tag length in bits (default 128).
    pub tag_length: Option<u32>,
    /// PBKDF2 iterations.
    pub iterations: Option<u32>,
    /// PBKDF2 salt.
    pub salt: Option<Uint8Array>,
    /// HKDF `info`.
    pub info: Option<Uint8Array>,
    /// RSA-OAEP label.
    pub label: Option<Uint8Array>,
    /// Associated hash.
    pub hash: Option<KeyAlgorithm>,
    /// RSA-PSS salt length in bytes.
    pub salt_length: Option<u32>,
    /// Peer public key for ECDH / X25519 derivation.
    pub public: Option<ClassInstance<CryptoKey>>,
    /// Requested output length in bits (deriveBits).
    pub length: Option<u32>,
}

/// Key material backing a [`CryptoKey`].
#[derive(Clone)]
enum KeyMaterial {
    /// Symmetric (AES / HMAC / ChaCha20-Poly1305 / AES-KW) raw bytes.
    Secret(Vec<u8>),
    /// Asymmetric private key.
    Private(PKey<Private>),
    /// Asymmetric public key.
    Public(PKey<Public>),
}

/// A Web Crypto `CryptoKey`.
#[napi]
pub struct CryptoKey {
    material: KeyMaterial,
    algorithm: CryptoKeyAlgorithm,
    extractable: bool,
    key_type: String,
    usages: Vec<String>,
    public_key: Option<Box<CryptoKey>>,
    private_key: Option<Box<CryptoKey>>,
}

/// The two halves of an asymmetric [`CryptoKey`].
#[napi]
pub struct CryptoKeyPair {
    public_key: CryptoKey,
    private_key: CryptoKey,
}

// ---------------------------------------------------------------------------
// Algorithm helpers
// ---------------------------------------------------------------------------

/// Which family an algorithm name belongs to.
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

fn digest_from_name(name: &str) -> Result<MessageDigest> {
    MessageDigest::from_name(name).ok_or_else(|| bad(format!("unsupported hash algorithm: {name}")))
}

fn hash_name(hash: &Option<KeyAlgorithm>) -> String {
    hash.as_ref()
        .map(|h| h.name.clone())
        .unwrap_or_else(|| "SHA-256".to_string())
}

fn curve_nid(name: &str) -> Result<Nid> {
    match name {
        "P-256" | "prime256v1" | "secp256r1" => Ok(Nid::X9_62_PRIME256V1),
        "P-384" | "secp384r1" => Ok(Nid::SECP384R1),
        "P-521" | "secp521r1" => Ok(Nid::SECP521R1),
        _ => Err(bad(format!("unsupported named curve: {name}"))),
    }
}

fn curve_name(nid: Nid) -> &'static str {
    match nid {
        Nid::X9_62_PRIME256V1 => "P-256",
        Nid::SECP384R1 => "P-384",
        Nid::SECP521R1 => "P-521",
        _ => "unknown",
    }
}

fn curve_oid(nid: Nid) -> &'static str {
    match nid {
        Nid::X9_62_PRIME256V1 => "1.2.840.10045.3.1.7",
        Nid::SECP384R1 => "1.3.132.0.34",
        Nid::SECP521R1 => "1.3.132.0.35",
        _ => "",
    }
}

fn curve_from_oid(oid: &str) -> Option<Nid> {
    match oid {
        "1.2.840.10045.3.1.7" => Some(Nid::X9_62_PRIME256V1),
        "1.3.132.0.34" => Some(Nid::SECP384R1),
        "1.3.132.0.35" => Some(Nid::SECP521R1),
        _ => None,
    }
}

fn aes_cipher(name: &str, bits: u32) -> Result<Cipher> {
    let c = match (name, bits) {
        ("AES-CBC", 128) => Cipher::aes_128_cbc(),
        ("AES-CBC", 192) => Cipher::aes_192_cbc(),
        ("AES-CBC", 256) => Cipher::aes_256_cbc(),
        ("AES-CTR", 128) => Cipher::aes_128_ctr(),
        ("AES-CTR", 192) => Cipher::aes_192_ctr(),
        ("AES-CTR", 256) => Cipher::aes_256_ctr(),
        ("AES-GCM", 128) => Cipher::aes_128_gcm(),
        ("AES-GCM", 192) => Cipher::aes_192_gcm(),
        ("AES-GCM", 256) => Cipher::aes_256_gcm(),
        ("AES-KW", 128) => Cipher::aes_128_ecb(),
        ("AES-KW", 192) => Cipher::aes_192_ecb(),
        ("AES-KW", 256) => Cipher::aes_256_ecb(),
        ("AES-OCB", 128) => Cipher::aes_128_ocb(),
        ("AES-OCB", 192) => Cipher::aes_192_ocb(),
        ("AES-OCB", 256) => Cipher::aes_256_ocb(),
        _ => {
            return Err(bad(format!(
                "unsupported AES variant/length: {name} ({bits} bits)"
            )));
        }
    };
    Ok(c)
}

fn is_aead(name: &str) -> bool {
    matches!(name, "AES-GCM" | "AES-OCB" | "ChaCha20-Poly1305")
}

fn allowed_usages(kind: AlgKind, alg_name: &str) -> &'static [&'static str] {
    match kind {
        AlgKind::Aes if alg_name == "AES-KW" => &["wrapKey", "unwrapKey"],
        AlgKind::Aes => &["encrypt", "decrypt", "wrapKey", "unwrapKey"],
        AlgKind::ChaCha => &["encrypt", "decrypt"],
        AlgKind::Mac => &["sign", "verify"],
        AlgKind::Ecdsa | AlgKind::Ed25519 | AlgKind::RsaPss | AlgKind::RsaPkcs1 => {
            &["sign", "verify"]
        }
        AlgKind::Ecdh | AlgKind::X25519 => &["deriveKey", "deriveBits"],
        AlgKind::RsaOaep => &["encrypt", "decrypt", "wrapKey", "unwrapKey"],
        AlgKind::Kdf => &["deriveKey", "deriveBits"],
    }
}

fn validate_usages(kind: AlgKind, alg_name: &str, usages: &[String]) -> Result<()> {
    if usages.is_empty() {
        return Err(bad("Usages cannot be empty"));
    }
    let allowed = allowed_usages(kind, alg_name);
    for u in usages {
        if !allowed.contains(&u.as_str()) {
            return Err(bad(format!(
                "Unsupported key usage '{u}' for {alg_name} (allowed: {})",
                allowed.join(", ")
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// DER (minimal SPKI only — PKCS#8 is handled by OpenSSL directly)
// ---------------------------------------------------------------------------

fn der_len_into(mut len: usize, out: &mut Vec<u8>) {
    if len < 0x80 {
        out.push(len as u8);
        return;
    }
    let mut tmp = [0u8; 9];
    let mut i = tmp.len();
    while len > 0 {
        i -= 1;
        tmp[i] = (len & 0xff) as u8;
        len >>= 8;
    }
    let n = tmp.len() - i;
    out.push(0x80 | n as u8);
    out.extend_from_slice(&tmp[i..]);
}

fn der_tlv(tag: u8, content: &[u8], out: &mut Vec<u8>) {
    out.push(tag);
    der_len_into(content.len(), out);
    out.extend_from_slice(content);
}

fn der_seq(items: &[&[u8]], out: &mut Vec<u8>) {
    let mut c = Vec::new();
    for it in items {
        c.extend_from_slice(it);
    }
    der_tlv(0x30, &c, out);
}

fn der_uint_be(bytes: &[u8], out: &mut Vec<u8>) {
    let mut s = bytes;
    while s.len() > 1 && s[0] == 0 {
        s = &s[1..];
    }
    if !s.is_empty() && s[0] & 0x80 != 0 {
        let mut c = Vec::with_capacity(s.len() + 1);
        c.push(0);
        c.extend_from_slice(s);
        der_tlv(0x02, &c, out);
    } else {
        der_tlv(0x02, s, out);
    }
}

fn der_null(out: &mut Vec<u8>) {
    der_tlv(0x05, &[], out);
}

fn der_oid(oid: &str, out: &mut Vec<u8>) {
    let parts: Vec<u32> = oid.split('.').filter_map(|p| p.parse().ok()).collect();
    let mut c = Vec::new();
    if parts.len() >= 2 {
        c.push((parts[0] * 40 + parts[1]).min(255) as u8);
    }
    for &arc in &parts[2..] {
        let mut tmp = [0u8; 5];
        let mut i = tmp.len();
        let mut v = arc;
        loop {
            i -= 1;
            tmp[i] = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                break;
            }
        }
        for j in i..(tmp.len() - 1) {
            tmp[j] |= 0x80;
        }
        c.extend_from_slice(&tmp[i..]);
    }
    der_tlv(0x06, &c, out);
}

fn der_bit_string(bytes: &[u8], out: &mut Vec<u8>) {
    let mut c = Vec::with_capacity(bytes.len() + 1);
    c.push(0); // no unused bits
    c.extend_from_slice(bytes);
    der_tlv(0x03, &c, out);
}

/// Reads one DER TLV, returning `(tag, content)` and advancing `pos`.
fn der_read_tlv<'a>(buf: &'a [u8], pos: &mut usize) -> Option<(u8, &'a [u8])> {
    if *pos >= buf.len() {
        return None;
    }
    let tag = buf[*pos];
    let mut i = *pos + 1;
    if i >= buf.len() {
        return None;
    }
    let first = buf[i];
    i += 1;
    let len: usize = if first < 0x80 {
        first as usize
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 || i + n > buf.len() {
            return None;
        }
        let mut l = 0usize;
        for _ in 0..n {
            l = (l << 8) | buf[i] as usize;
            i += 1;
        }
        l
    };
    let end = i.checked_add(len)?;
    if end > buf.len() {
        return None;
    }
    *pos = end;
    Some((tag, &buf[i..end]))
}

fn decode_oid(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = vec![(bytes[0] / 40).to_string(), (bytes[0] % 40).to_string()];
    let mut i = 1;
    while i < bytes.len() {
        let mut v: u64 = 0;
        loop {
            if i >= bytes.len() {
                return None;
            }
            let b = bytes[i];
            i += 1;
            v = (v << 7) | (b & 0x7f) as u64;
            if b & 0x80 == 0 {
                break;
            }
        }
        parts.push(v.to_string());
    }
    Some(parts.join("."))
}

/// Encode a public key as a SubjectPublicKeyInfo (SPKI) DER blob.
fn spki_encode(pkey: &PKey<Public>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    if let Some(ec) = pkey.ec_key() {
        let nid = ec
            .group()
            .curve_name()
            .ok_or_else(|| fail("cannot determine the key's curve"))?;
        let mut oid_alg = Vec::new();
        der_oid("1.2.840.10045.2.1", &mut oid_alg);
        let mut oid_crv = Vec::new();
        der_oid(curve_oid(nid), &mut oid_crv);
        let mut algid = Vec::new();
        der_seq(&[&oid_alg, &oid_crv], &mut algid);
        let ctx = BigNumContext::new()?;
        let point =
            ec.public_key()
                .to_bytes(ec.group(), PointConversionForm::UNCOMPRESSED, &ctx)?;
        let mut bits = Vec::new();
        der_bit_string(&point, &mut bits);
        der_seq(&[&algid, &bits], &mut out);
    } else if let Some(rsa) = pkey.rsa() {
        let mut oid_alg = Vec::new();
        der_oid("1.2.840.113549.1.1.1", &mut oid_alg);
        let mut null = Vec::new();
        der_null(&mut null);
        let mut algid = Vec::new();
        der_seq(&[&oid_alg, &null], &mut algid);
        let mut n = Vec::new();
        der_uint_be(&rsa.n().to_vec(), &mut n);
        let mut e = Vec::new();
        der_uint_be(&rsa.e().to_vec(), &mut e);
        let mut rk = Vec::new();
        der_seq(&[&n, &e], &mut rk);
        let mut bits = Vec::new();
        der_bit_string(&rk, &mut bits);
        der_seq(&[&algid, &bits], &mut out);
    } else {
        let oid = match pkey.id() {
            Id::ED25519 => "1.3.101.112",
            Id::X25519 => "1.3.101.110",
            _ => return Err(fail("unsupported key type for SPKI export")),
        };
        let mut oid_alg = Vec::new();
        der_oid(oid, &mut oid_alg);
        let mut algid = Vec::new();
        der_seq(&[&oid_alg], &mut algid);
        let mut bits = Vec::new();
        der_bit_string(&pkey.raw_public_key()?, &mut bits);
        der_seq(&[&algid, &bits], &mut out);
    }
    Ok(out)
}

/// Parse a SubjectPublicKeyInfo (SPKI) DER blob into a public key.
fn spki_decode(der: &[u8]) -> Result<PKey<Public>> {
    let mut p = 0;
    let (tag, content) =
        der_read_tlv(der, &mut p).ok_or_else(|| bad("malformed SPKI: not a SEQUENCE"))?;
    if tag != 0x30 {
        return Err(bad("malformed SPKI: outer structure is not a SEQUENCE"));
    }
    let mut q = 0;
    let (atag, algid) = der_read_tlv(content, &mut q)
        .ok_or_else(|| bad("malformed SPKI: missing AlgorithmIdentifier"))?;
    if atag != 0x30 {
        return Err(bad("malformed SPKI: AlgorithmIdentifier is not a SEQUENCE"));
    }
    let (btag, bits) = der_read_tlv(content, &mut q)
        .ok_or_else(|| bad("malformed SPKI: missing subjectPublicKey"))?;
    if btag != 0x03 || bits.is_empty() {
        return Err(bad("malformed SPKI: subjectPublicKey is not a BIT STRING"));
    }
    let key_bytes = &bits[1..]; // skip the unused-bits octet

    let mut r = 0;
    let (otag, oid_bytes) =
        der_read_tlv(algid, &mut r).ok_or_else(|| bad("malformed SPKI: missing algorithm OID"))?;
    if otag != 0x06 {
        return Err(bad("malformed SPKI: algorithm OID missing"));
    }
    let oid = decode_oid(oid_bytes).ok_or_else(|| bad("malformed SPKI: undecodable OID"))?;
    let params = der_read_tlv(algid, &mut r);

    match oid.as_str() {
        "1.2.840.10045.2.1" => {
            let crv = params
                .and_then(|(t, c)| if t == 0x06 { decode_oid(c) } else { None })
                .ok_or_else(|| bad("malformed SPKI: EC curve parameter missing"))?;
            let nid =
                curve_from_oid(&crv).ok_or_else(|| bad(format!("unsupported curve OID: {crv}")))?;
            let group = EcGroup::from_curve_name(nid)?;
            let ctx = BigNumContext::new()?;
            let point = EcPoint::from_bytes(&group, key_bytes, &ctx)?;
            let ec = EcKey::from_public_key(&group, &point)?;
            Ok(PKey::from_ec_key(ec)?)
        }
        "1.2.840.113549.1.1.1" => {
            let mut s = 0;
            let (_, seq) = der_read_tlv(key_bytes, &mut s)
                .ok_or_else(|| bad("malformed SPKI: RSAPublicKey is not a SEQUENCE"))?;
            let mut t = 0;
            let (_, n_b) = der_read_tlv(seq, &mut t)
                .ok_or_else(|| bad("malformed SPKI: RSA modulus missing"))?;
            let (_, e_b) = der_read_tlv(seq, &mut t)
                .ok_or_else(|| bad("malformed SPKI: RSA exponent missing"))?;
            let n = BigNum::from_slice(n_b)?;
            let e = BigNum::from_slice(e_b)?;
            Ok(PKey::from_rsa(Rsa::from_public_components(&n, &e)?)?)
        }
        "1.3.101.112" => Ok(PKey::public_key_from_raw_bytes(key_bytes, Id::ED25519)?),
        "1.3.101.110" => Ok(PKey::public_key_from_raw_bytes(key_bytes, Id::X25519)?),
        _ => Err(bad(format!("unsupported SPKI algorithm OID: {oid}"))),
    }
}

fn aead_seal(cipher: Cipher, key: &[u8], iv: &[u8], aad: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let mut c = Crypter::new(cipher, Mode::Encrypt, key, Some(iv))?;
    c.pad(false);
    if !aad.is_empty() {
        c.aad_update(aad)?;
    }
    let mut out = vec![0u8; data.len() + cipher.block_size()];
    let mut n = c.update(data, &mut out)?;
    n += c.finalize(&mut out[n..])?;
    out.truncate(n);
    let mut tag = [0u8; 16];
    c.get_tag(&mut tag)?;
    out.extend_from_slice(&tag);
    Ok(out)
}

fn aead_open(cipher: Cipher, key: &[u8], iv: &[u8], aad: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 16 {
        return Err(bad("ciphertext too short to contain an authentication tag"));
    }
    let (ct, tag) = data.split_at(data.len() - 16);
    let mut c = Crypter::new(cipher, Mode::Decrypt, key, Some(iv))?;
    c.pad(false);
    c.set_tag_len(16)?;
    c.set_tag(tag)?;
    if !aad.is_empty() {
        c.aad_update(aad)?;
    }
    let mut out = vec![0u8; ct.len() + cipher.block_size()];
    let mut n = c.update(ct, &mut out)?;
    n += c.finalize(&mut out[n..])?;
    out.truncate(n);
    Ok(out)
}

fn block_crypt(cipher: Cipher, mode: Mode, key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let mut c = Crypter::new(cipher, mode, key, Some(iv))?;
    c.pad(false);
    let mut out = vec![0u8; data.len() + cipher.block_size()];
    let mut n = c.update(data, &mut out)?;
    n += c.finalize(&mut out[n..])?;
    out.truncate(n);
    Ok(out)
}

fn aes_ecb_raw(cipher: Cipher, mode: Mode, key: &[u8], block: &[u8]) -> Result<Vec<u8>> {
    let mut c = Crypter::new(cipher, mode, key, None)?;
    c.pad(false);
    let mut out = vec![0u8; block.len() + cipher.block_size()];
    let mut n = c.update(block, &mut out)?;
    n += c.finalize(&mut out[n..])?;
    out.truncate(n);
    Ok(out)
}

/// RFC 3394 AES key wrap.
fn aes_kw_wrap(kek: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    if plaintext.len() % 8 != 0 || plaintext.len() < 16 {
        return Err(bad(
            "AES-KW key material must be a multiple of 8 bytes and at least 16 bytes",
        ));
    }
    let n = plaintext.len() / 8;
    let cipher = aes_cipher("AES-KW", (kek.len() * 8) as u32)?;
    let mut blocks: Vec<[u8; 8]> = plaintext
        .chunks_exact(8)
        .map(|c| {
            let mut b = [0u8; 8];
            b.copy_from_slice(c);
            b
        })
        .collect();
    let mut a = [0xA6u8; 8]; // RFC 3394 default IV
    for i in 1..=6u64 {
        for (j, blk) in blocks.iter_mut().enumerate() {
            let mut input = Vec::with_capacity(16);
            input.extend_from_slice(&a);
            input.extend_from_slice(blk);
            let enc = aes_ecb_raw(cipher, Mode::Encrypt, kek, &input)?;
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

/// RFC 3394 AES key unwrap.
fn aes_kw_unwrap(kek: &[u8], wrapped: &[u8]) -> Result<Vec<u8>> {
    if wrapped.len() % 8 != 0 || wrapped.len() < 24 {
        return Err(bad(
            "AES-KW wrapped key must be a multiple of 8 bytes and at least 24 bytes",
        ));
    }
    let n = wrapped.len() / 8 - 1;
    let cipher = aes_cipher("AES-KW", (kek.len() * 8) as u32)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(&wrapped[..8]);
    let mut blocks: Vec<[u8; 8]> = wrapped[8..]
        .chunks_exact(8)
        .map(|c| {
            let mut b = [0u8; 8];
            b.copy_from_slice(c);
            b
        })
        .collect();
    for i in (1..=6u64).rev() {
        for j in (0..n).rev() {
            let cnt = (i * n as u64 + j as u64).to_be_bytes();
            let mut at = a;
            for k in 0..8 {
                at[k] ^= cnt[k];
            }
            let mut input = Vec::with_capacity(16);
            input.extend_from_slice(&at);
            input.extend_from_slice(&blocks[j]);
            let dec = aes_ecb_raw(cipher, Mode::Decrypt, kek, &input)?;
            a.copy_from_slice(&dec[..8]);
            blocks[j].copy_from_slice(&dec[8..]);
        }
    }
    if a != [0xA6u8; 8] {
        return Err(fail("AES-KW integrity check failed"));
    }
    let mut out = Vec::with_capacity(n * 8);
    for b in &blocks {
        out.extend_from_slice(b);
    }
    Ok(out)
}

fn public_from_private(p: &PKey<Private>) -> Result<PKey<Public>> {
    if let Some(ec) = p.ec_key() {
        let ec_pub = EcKey::from_public_key(ec.group(), ec.public_key())?;
        return Ok(PKey::from_ec_key(ec_pub)?);
    }
    if let Some(rsa) = p.rsa() {
        let n = BigNum::from_slice(&rsa.n().to_vec())?;
        let e = BigNum::from_slice(&rsa.e().to_vec())?;
        return Ok(PKey::from_rsa(Rsa::from_public_components(&n, &e)?)?);
    }
    PKey::public_key_from_raw_bytes(&p.raw_public_key()?, p.id())
}

fn fill_derived(alg: &mut CryptoKeyAlgorithm, material: &KeyMaterial) {
    match material {
        KeyMaterial::Secret(b) => alg.length = Some((b.len() * 8) as u32),
        KeyMaterial::Private(p) | KeyMaterial::Public(p) => {
            if let Some(ec) = p.ec_key() {
                if let Some(nid) = ec.group().curve_name() {
                    alg.named_curve = Some(curve_name(nid).to_string());
                }
            } else if let Some(rsa) = p.rsa() {
                alg.modulus_length = Some(p.bits());
                alg.public_exponent = Some(Uint8Array::new(rsa.e().to_vec()));
            }
            if alg.hash.is_none()
                && matches!(
                    alg.name.as_str(),
                    "RSA-OAEP" | "RSA-PSS" | "RSASSA-PKCS1-v1_5"
                )
            {
                alg.hash = Some(KeyAlgorithm {
                    name: "SHA-256".to_string(),
                });
            }
        }
    }
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

fn b64url_decode(s: &str) -> Result<Vec<u8>> {
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

fn jwk_alg_name(alg: &CryptoKeyAlgorithm) -> String {
    match alg.name.as_str() {
        "HMAC" => format!("HS{}", alg.length.unwrap_or(256)),
        "ECDSA" => format!(
            "ES{}",
            alg.named_curve
                .as_deref()
                .unwrap_or("256")
                .trim_start_matches('P')
        ),
        "RSASSA-PKCS1-v1_5" => "RS256".to_string(),
        "RSA-PSS" => "PS256".to_string(),
        "RSA-OAEP" => "RSA-OAEP".to_string(),
        n => n.to_string(),
    }
}

fn jwk_encode(key: &CryptoKey) -> Result<serde_json::Value> {
    use serde_json::{Map, Value, json};
    let mut m = Map::new();
    match &key.material {
        KeyMaterial::Secret(b) => {
            m.insert("kty".into(), json!("oct"));
            m.insert("k".into(), json!(b64url_encode(b)));
        }
        KeyMaterial::Private(p) | KeyMaterial::Public(p) => {
            let is_private = matches!(key.material, KeyMaterial::Private(_));
            if let Some(ec) = p.ec_key() {
                let nid = ec
                    .group()
                    .curve_name()
                    .ok_or_else(|| fail("cannot determine the key's curve"))?;
                let ctx = BigNumContext::new()?;
                let point = ec.public_key().to_bytes(
                    ec.group(),
                    PointConversionForm::UNCOMPRESSED,
                    &ctx,
                )?;
                let coord_len = (point.len() - 1) / 2;
                m.insert("kty".into(), json!("EC"));
                m.insert("crv".into(), json!(curve_name(nid)));
                m.insert("x".into(), json!(b64url_encode(&point[1..1 + coord_len])));
                m.insert("y".into(), json!(b64url_encode(&point[1 + coord_len..])));
                if is_private {
                    m.insert("d".into(), json!(b64url_encode(&ec.private_key().to_vec())));
                }
            } else if let Some(rsa) = p.rsa() {
                m.insert("kty".into(), json!("RSA"));
                m.insert("n".into(), json!(b64url_encode(&rsa.n().to_vec())));
                m.insert("e".into(), json!(b64url_encode(&rsa.e().to_vec())));
                if is_private {
                    m.insert("d".into(), json!(b64url_encode(&rsa.d().to_vec())));
                    m.insert("p".into(), json!(b64url_encode(&rsa.p().to_vec())));
                    m.insert("q".into(), json!(b64url_encode(&rsa.q().to_vec())));
                    m.insert("dp".into(), json!(b64url_encode(&rsa.dmp1().to_vec())));
                    m.insert("dq".into(), json!(b64url_encode(&rsa.dmq1().to_vec())));
                    m.insert("qi".into(), json!(b64url_encode(&rsa.iqmp().to_vec())));
                }
            } else {
                let crv = match p.id() {
                    Id::ED25519 => "Ed25519",
                    Id::X25519 => "X25519",
                    _ => return Err(fail("unsupported key type for JWK export")),
                };
                m.insert("kty".into(), json!("OKP"));
                m.insert("crv".into(), json!(crv));
                m.insert("x".into(), json!(b64url_encode(&p.raw_public_key()?)));
                if is_private {
                    m.insert("d".into(), json!(b64url_encode(&p.raw_private_key()?)));
                }
            }
        }
    }
    m.insert("alg".into(), json!(jwk_alg_name(&key.algorithm)));
    m.insert("ext".into(), json!(key.extractable));
    m.insert(
        "key_ops".into(),
        json!(key.usages.iter().cloned().collect::<Vec<_>>()),
    );
    Ok(Value::Object(m))
}

fn jwk_str(jwk: &serde_json::Value, field: &str) -> Result<String> {
    jwk.get(field)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| bad(format!("JWK is missing the '{field}' member")))
}

fn ec_point_from_coords(group: &EcGroupRef, x: &[u8], y: &[u8]) -> Result<EcPoint> {
    let mut buf = Vec::with_capacity(1 + x.len() + y.len());
    buf.push(0x04); // uncompressed
    buf.extend_from_slice(x);
    buf.extend_from_slice(y);
    let ctx = BigNumContext::new()?;
    Ok(EcPoint::from_bytes(group, &buf, &ctx)?)
}

fn jwk_decode(
    jwk: &serde_json::Value,
    algorithm: CryptoKeyAlgorithm,
    extractable: bool,
    usages: Vec<String>,
) -> Result<CryptoKey> {
    let kty = jwk_str(jwk, "kty")?;
    let kind = alg_kind(&algorithm.name)
        .ok_or_else(|| bad(format!("unsupported algorithm: {}", algorithm.name)))?;
    validate_usages(kind, &algorithm.name, &usages)?;
    let mut alg = algorithm;

    let (material, key_type) = match kty.as_str() {
        "oct" => {
            let k = b64url_decode(&jwk_str(jwk, "k")?)?;
            (KeyMaterial::Secret(k), "secret")
        }
        "EC" => {
            let nid = curve_nid(&jwk_str(jwk, "crv")?)?;
            let group = EcGroup::from_curve_name(nid)?;
            let x = b64url_decode(&jwk_str(jwk, "x")?)?;
            let y = b64url_decode(&jwk_str(jwk, "y")?)?;
            let point = ec_point_from_coords(&group, &x, &y)?;
            match jwk.get("d").and_then(|v| v.as_str()) {
                Some(ds) => {
                    let d = b64url_decode(ds)?;
                    let ec = EcKey::from_private_components(
                        &group,
                        &BigNum::from_slice(&d)?,
                        Some(&point),
                    )?;
                    (KeyMaterial::Private(PKey::from_ec_key(ec)?), "private")
                }
                None => {
                    let ec = EcKey::from_public_key(&group, &point)?;
                    (KeyMaterial::Public(PKey::from_ec_key(ec)?), "public")
                }
            }
        }
        "RSA" => {
            let n = BigNum::from_slice(&b64url_decode(&jwk_str(jwk, "n")?)?)?;
            let e = BigNum::from_slice(&b64url_decode(&jwk_str(jwk, "e")?)?)?;
            match jwk.get("d").and_then(|v| v.as_str()) {
                Some(ds) => {
                    let d = b64url_decode(ds)?;
                    let rsa = Rsa::from_private_components(
                        &n,
                        &e,
                        &BigNum::from_slice(&d)?,
                        &BigNum::from_slice(&b64url_decode(&jwk_str(jwk, "p")?)?)?,
                        &BigNum::from_slice(&b64url_decode(&jwk_str(jwk, "q")?)?)?,
                        &BigNum::from_slice(&b64url_decode(&jwk_str(jwk, "dp")?)?)?,
                        &BigNum::from_slice(&b64url_decode(&jwk_str(jwk, "dq")?)?)?,
                        &BigNum::from_slice(&b64url_decode(&jwk_str(jwk, "qi")?)?)?,
                    )?;
                    (KeyMaterial::Private(PKey::from_rsa(rsa)?), "private")
                }
                None => (
                    KeyMaterial::Public(PKey::from_rsa(Rsa::from_public_components(&n, &e)?)?),
                    "public",
                ),
            }
        }
        "OKP" => {
            let crv = jwk_str(jwk, "crv")?;
            let id = match crv.as_str() {
                "Ed25519" => Id::ED25519,
                "X25519" => Id::X25519,
                _ => return Err(bad(format!("unsupported OKP curve: {crv}"))),
            };
            let x = b64url_decode(&jwk_str(jwk, "x")?)?;
            match jwk.get("d").and_then(|v| v.as_str()) {
                Some(ds) => {
                    let d = b64url_decode(ds)?;
                    let p = PKey::private_key_from_raw_bytes(&d, id)?;
                    (KeyMaterial::Private(p), "private")
                }
                None => (
                    KeyMaterial::Public(PKey::public_key_from_raw_bytes(&x, id)?),
                    "public",
                ),
            }
        }
        other => return Err(bad(format!("unsupported JWK key type: {other}"))),
    };

    fill_derived(&mut alg, &material);
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

// ---------------------------------------------------------------------------
// Operation helpers
// ---------------------------------------------------------------------------

fn hmac(md: MessageDigest, key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let pkey = PKey::hmac(key)?;
    let mut s = Signer::new(md, &pkey)?;
    s.update(data)?;
    Ok(s.sign_to_vec()?)
}

/// Manual HKDF (extract + expand) — avoids the opaque blsful kdf helper surface.
fn hkdf(
    md: MessageDigest,
    ikm: &[u8],
    salt: Option<&[u8]>,
    info: Option<&[u8]>,
    out: &mut [u8],
) -> Result<()> {
    let hashlen = md.size();
    let empty = vec![0u8; hashlen];
    let salt = salt.unwrap_or(&empty);
    let info = info.unwrap_or(&[]);
    // Extract
    let prk = hmac(md, salt, ikm)?;
    // Expand
    let mut acc = Vec::new();
    let mut t = Vec::new();
    let mut counter: u8 = 1;
    while acc.len() < out.len() {
        let mut input = Vec::with_capacity(t.len() + info.len() + 1);
        input.extend_from_slice(&t);
        input.extend_from_slice(info);
        input.push(counter);
        t = hmac(md, &prk, &input)?;
        acc.extend_from_slice(&t);
        counter = counter.wrapping_add(1);
        if counter == 0 {
            break;
        }
    }
    out.copy_from_slice(&acc[..out.len()]);
    Ok(())
}

fn sign_impl(
    kind: AlgKind,
    pkey: &PKey<Private>,
    hash_name: &str,
    salt_len: Option<u32>,
    data: &[u8],
) -> Result<Vec<u8>> {
    let mut s = match kind {
        AlgKind::Ed25519 => Signer::new_without_digest(pkey)?,
        AlgKind::Ecdsa => Signer::new(digest_from_name(hash_name)?, pkey)?,
        AlgKind::RsaPss => {
            let mut s = Signer::new(digest_from_name(hash_name)?, pkey)?;
            s.set_rsa_padding(Padding::PKCS1_PSS)?;
            s.set_rsa_pss_saltlen(RsaPssSaltlen::DIGEST_LENGTH)?;
            s
        }
        AlgKind::RsaPkcs1 => Signer::new(digest_from_name(hash_name)?, pkey)?,
        _ => return Err(bad("algorithm does not support signing")),
    };
    s.update(data)?;
    Ok(s.sign_to_vec()?)
}

fn verify_impl(
    kind: AlgKind,
    pkey: &PKey<Public>,
    hash_name: &str,
    data: &[u8],
    signature: &[u8],
) -> Result<bool> {
    let mut v = match kind {
        AlgKind::Ed25519 => Verifier::new_without_digest(pkey)?,
        AlgKind::Ecdsa => Verifier::new(digest_from_name(hash_name)?, pkey)?,
        AlgKind::RsaPss => {
            let mut v = Verifier::new(digest_from_name(hash_name)?, pkey)?;
            v.set_rsa_padding(Padding::PKCS1_PSS)?;
            v.set_rsa_pss_saltlen(RsaPssSaltlen::DIGEST_LENGTH)?;
            v
        }
        AlgKind::RsaPkcs1 => Verifier::new(digest_from_name(hash_name)?, pkey)?,
        _ => return Err(bad("algorithm does not support verification")),
    };
    v.update(data)?;
    Ok(v.verify(signature)?)
}

fn encrypt_impl(params: &CryptoOpParams, key: &CryptoKey, data: &[u8]) -> Result<Vec<u8>> {
    let name = params
        .name
        .clone()
        .ok_or_else(|| bad("encrypt requires algorithm.name"))?;
    let kind = alg_kind(&name).ok_or_else(|| bad(format!("unsupported algorithm: {name}")))?;
    key.check_usage("encrypt")?;
    let iv = params.iv.as_ref().map(|u| u.as_ref().to_vec());
    let aad = params
        .additional_data
        .as_ref()
        .map(|u| u.as_ref().to_vec())
        .unwrap_or_default();

    match (&key.material, kind) {
        (KeyMaterial::Secret(k), AlgKind::Aes) => {
            let bits = (k.len() * 8) as u32;
            let cipher = aes_cipher(&name, bits)?;
            if is_aead(&name) {
                let iv = iv.ok_or_else(|| bad(format!("{name} requires an iv")))?;
                aead_seal(cipher, k, &iv, &aad, data)
            } else if name == "AES-KW" {
                Err(bad("AES-KW cannot be used with encrypt; use wrapKey"))
            } else {
                let iv = iv.ok_or_else(|| bad(format!("{name} requires an iv")))?;
                block_crypt(cipher, Mode::Encrypt, k, &iv, data)
            }
        }
        (KeyMaterial::Secret(k), AlgKind::ChaCha) => {
            let iv = iv.ok_or_else(|| bad("ChaCha20-Poly1305 requires an iv"))?;
            aead_seal(Cipher::chacha20_poly1305(), k, &iv, &aad, data)
        }
        (KeyMaterial::Public(p), AlgKind::RsaOaep)
        | (KeyMaterial::Private(p), AlgKind::RsaOaep) => {
            let md = digest_from_name(&hash_name(&params.hash))?;
            let mut e = Encrypter::new(p)?;
            e.set_rsa_padding(Padding::PKCS1_OAEP)?;
            e.set_rsa_oaep_md(md)?;
            if let Some(l) = &params.label {
                if !l.is_empty() {
                    e.set_rsa_oaep_label(l.as_ref())?;
                }
            }
            Ok(e.encrypt(data)?)
        }
        _ => Err(bad(format!(
            "{name} cannot be used to encrypt with this key"
        ))),
    }
}

fn decrypt_impl(params: &CryptoOpParams, key: &CryptoKey, data: &[u8]) -> Result<Vec<u8>> {
    let name = params
        .name
        .clone()
        .ok_or_else(|| bad("decrypt requires algorithm.name"))?;
    let kind = alg_kind(&name).ok_or_else(|| bad(format!("unsupported algorithm: {name}")))?;
    key.check_usage("decrypt")?;
    let iv = params.iv.as_ref().map(|u| u.as_ref().to_vec());
    let aad = params
        .additional_data
        .as_ref()
        .map(|u| u.as_ref().to_vec())
        .unwrap_or_default();

    match (&key.material, kind) {
        (KeyMaterial::Secret(k), AlgKind::Aes) => {
            let bits = (k.len() * 8) as u32;
            let cipher = aes_cipher(&name, bits)?;
            if is_aead(&name) {
                let iv = iv.ok_or_else(|| bad(format!("{name} requires an iv")))?;
                aead_open(cipher, k, &iv, &aad, data)
            } else if name == "AES-KW" {
                Err(bad("AES-KW cannot be used with decrypt; use unwrapKey"))
            } else {
                let iv = iv.ok_or_else(|| bad(format!("{name} requires an iv")))?;
                block_crypt(cipher, Mode::Decrypt, k, &iv, data)
            }
        }
        (KeyMaterial::Secret(k), AlgKind::ChaCha) => {
            let iv = iv.ok_or_else(|| bad("ChaCha20-Poly1305 requires an iv"))?;
            aead_open(Cipher::chacha20_poly1305(), k, &iv, &aad, data)
        }
        (KeyMaterial::Private(p), AlgKind::RsaOaep) => {
            let md = digest_from_name(&hash_name(&params.hash))?;
            let mut d = Decrypter::new(p)?;
            d.set_rsa_padding(Padding::PKCS1_OAEP)?;
            d.set_rsa_oaep_md(md)?;
            if let Some(l) = &params.label {
                if !l.is_empty() {
                    d.set_rsa_oaep_label(l.as_ref())?;
                }
            }
            Ok(d.decrypt(data)?)
        }
        _ => Err(bad(format!(
            "{name} cannot be used to decrypt with this key"
        ))),
    }
}

/// HMAC over `data` using `password`, backed by blsful (OpenSSL). Keeps the
/// PBKDF2 path free of a direct `blsful` crate dependency.
fn bls_hmac(md: MessageDigest, password: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let pkey = PKey::hmac(password)?;
    let mut signer = Signer::new(md, &pkey)?;
    signer.update(data)?;
    Ok(signer.sign_to_vec()?)
}

/// PBKDF2-HMAC implemented on top of [`bls_hmac`], replacing `blsful::pkcs5`.
fn pbkdf2_hmac_bl(
    md: MessageDigest,
    password: &[u8],
    salt: &[u8],
    iter: usize,
    out: &mut [u8],
) -> Result<()> {
    let hlen = md.size();
    if hlen == 0 {
        return Err(bad("unsupported digest for PBKDF2"));
    }
    let mut i = 1u32;
    let mut offset = 0usize;
    while offset < out.len() {
        let mut msg = Vec::with_capacity(salt.len() + 4);
        msg.extend_from_slice(salt);
        msg.extend_from_slice(&i.to_be_bytes());
        let mut u = bls_hmac(md, password, &msg)?;
        let mut t = u.clone();
        for _ in 1..iter {
            u = bls_hmac(md, password, &u)?;
            for (a, b) in t.iter_mut().zip(u.iter()) {
                *a ^= *b;
            }
        }
        let take = std::cmp::min(hlen, out.len() - offset);
        out[offset..offset + take].copy_from_slice(&t[..take]);
        offset += take;
        i += 1;
    }
    Ok(())
}

fn derive_bits_impl(
    params: &CryptoOpParams,
    base_key: &CryptoKey,
    length: Option<u32>,
) -> Result<Vec<u8>> {
    let name = params
        .name
        .clone()
        .ok_or_else(|| bad("deriveBits requires algorithm.name"))?;
    let kind = alg_kind(&name).ok_or_else(|| bad(format!("unsupported algorithm: {name}")))?;
    // Web Crypto allows either "deriveKey" or "deriveBits" usage for derivation.
    if !base_key
        .usages
        .iter()
        .any(|u| u == "deriveBits" || u == "deriveKey")
    {
        return Err(bad(
            "The requested operation is not valid for this key; 'deriveKey'/'deriveBits' is not in its usages",
        ));
    }
    let md = digest_from_name(&hash_name(&params.hash))?;

    match (&base_key.material, kind) {
        (KeyMaterial::Secret(k), AlgKind::Kdf) => match name.as_str() {
            "PBKDF2" => {
                let salt = params
                    .salt
                    .as_ref()
                    .ok_or_else(|| bad("PBKDF2 requires a salt"))?;
                let iters = params.iterations.unwrap_or(100_000);
                let len = length.ok_or_else(|| bad("PBKDF2 deriveBits requires a length"))?;
                let mut out = vec![0u8; (len / 8) as usize];
                pbkdf2_hmac_bl(md, k, salt.as_ref(), iters as usize, &mut out)?;
                Ok(out)
            }
            "HKDF" => {
                let len = length.ok_or_else(|| bad("HKDF deriveBits requires a length"))?;
                let mut out = vec![0u8; (len / 8) as usize];
                hkdf(
                    md,
                    k,
                    params.salt.as_ref().map(|s| s.as_ref()),
                    params.info.as_ref().map(|i| i.as_ref()),
                    &mut out,
                )?;
                Ok(out)
            }
            _ => Err(bad(format!("{name} is not supported for derivation"))),
        },
        (KeyMaterial::Private(p), AlgKind::Ecdh) | (KeyMaterial::Public(p), AlgKind::Ecdh) => {
            let peer = params
                .public
                .as_ref()
                .ok_or_else(|| bad("ECDH requires the peer public key as algorithm.public"))?;
            let peer_pub = public_from_private_peer(peer)?;
            let mut ctx = PkeyCtx::new(p)?;
            ctx.derive_init()?;
            ctx.derive_set_peer(&peer_pub)?;
            let len = (p.bits() as usize + 7) / 8;
            let mut out = vec![0u8; len];
            ctx.derive(Some(&mut out))?;
            Ok(out)
        }
        (KeyMaterial::Private(p), AlgKind::X25519) => {
            let peer = params
                .public
                .as_ref()
                .ok_or_else(|| bad("X25519 requires the peer public key as algorithm.public"))?;
            let peer_pub = public_from_private_peer(peer)?;
            let mut ctx = PkeyCtx::new(p)?;
            ctx.derive_init()?;
            ctx.derive_set_peer(&peer_pub)?;
            let mut out = vec![0u8; 32];
            ctx.derive(Some(&mut out))?;
            Ok(out)
        }
        _ => Err(bad(format!(
            "{name} cannot be used for derivation with this key"
        ))),
    }
}

fn public_from_private_peer(peer: &CryptoKey) -> Result<PKey<Public>> {
    match &peer.material {
        KeyMaterial::Public(p) => Ok(p.clone()),
        KeyMaterial::Private(p) => public_from_private(p),
        KeyMaterial::Secret(_) => Err(bad("a symmetric key cannot be an ECDH peer")),
    }
}

fn to_public_of(key: &CryptoKey) -> Result<PKey<Public>> {
    match &key.material {
        KeyMaterial::Public(p) => Ok(p.clone()),
        KeyMaterial::Private(p) => public_from_private(p),
        KeyMaterial::Secret(_) => Err(bad("symmetric keys have no public half")),
    }
}

fn to_private_of(key: &CryptoKey) -> Result<PKey<Private>> {
    match &key.material {
        KeyMaterial::Private(p) => Ok(p.clone()),
        _ => Err(bad("operation requires a private key")),
    }
}

fn to_secret_of(key: &CryptoKey) -> Result<Vec<u8>> {
    match &key.material {
        KeyMaterial::Secret(b) => Ok(b.clone()),
        _ => Err(bad("operation requires a symmetric (secret) key")),
    }
}

fn to_private_of_key(material: &KeyMaterial) -> Result<PKey<Private>> {
    match material {
        KeyMaterial::Private(p) => Ok(p.clone()),
        _ => Err(bad("expected a private key")),
    }
}

// ---------------------------------------------------------------------------
// The class
// ---------------------------------------------------------------------------

/// Normalize an algorithm argument that may be given either as a bare name
/// string (Node allows `"AES-GCM"`) or as a full algorithm dictionary.
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

#[napi]
impl CryptoKey {
    /// Generate a new key (or key pair) for `algorithm`.
    ///
    /// Asymmetric generation returns a `CryptoKey` whose `publicKey` /
    /// `privateKey` getters are populated, so `const { publicKey, privateKey } =
    /// await new CryptoKey(...)` works.
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
        validate_usages(kind, &algorithm.name, &usages)?;

        let material = match kind {
            AlgKind::Aes | AlgKind::ChaCha => {
                let bits = algorithm.length.unwrap_or(256);
                let mut b = vec![0u8; (bits / 8) as usize];
                rand::rng().fill_bytes(&mut b);
                KeyMaterial::Secret(b)
            }
            AlgKind::Mac => {
                let len = match algorithm.length {
                    Some(l) => (l / 8) as usize,
                    None => digest_from_name(&hash_name(&algorithm.hash))?.size(),
                };
                let mut b = vec![0u8; len];
                rand::rng().fill_bytes(&mut b);
                KeyMaterial::Secret(b)
            }
            AlgKind::Ecdsa | AlgKind::Ecdh => {
                let curve = algorithm
                    .named_curve
                    .clone()
                    .unwrap_or_else(|| "P-256".to_string());
                let group = EcGroup::from_curve_name(curve_nid(&curve)?)?;
                KeyMaterial::Private(PKey::from_ec_key(EcKey::generate(&group)?)?)
            }
            AlgKind::Ed25519 => KeyMaterial::Private(PKey::generate_ed25519()?),
            AlgKind::X25519 => KeyMaterial::Private(PKey::generate_x25519()?),
            AlgKind::RsaOaep | AlgKind::RsaPss | AlgKind::RsaPkcs1 => {
                let bits = algorithm.modulus_length.unwrap_or(2048);
                let e_bytes = algorithm
                    .public_exponent
                    .as_ref()
                    .map(|u| u.as_ref().to_vec())
                    .unwrap_or_else(|| vec![1, 0, 1]);
                let e = BigNum::from_slice(&e_bytes)?;
                KeyMaterial::Private(PKey::from_rsa(Rsa::generate_with_e(bits, &e)?)?)
            }
            AlgKind::Kdf => {
                return Err(bad(format!(
                    "{} does not support key generation; use importKey",
                    algorithm.name
                )));
            }
        };

        if is_symmetric(kind) {
            fill_derived(&mut algorithm, &material);
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

        let pkey = to_private_of_key(&material)?;
        let public = public_from_private(&pkey)?;
        fill_derived(&mut algorithm, &KeyMaterial::Private(pkey.clone()));
        let mut pub_alg = algorithm.clone();
        fill_derived(&mut pub_alg, &KeyMaterial::Public(public.clone()));

        let pub_key = Self {
            material: KeyMaterial::Public(public),
            algorithm: pub_alg,
            extractable,
            key_type: "public".to_string(),
            usages: usages.clone(),
            public_key: None,
            private_key: None,
        };
        let priv_key = Self {
            material: KeyMaterial::Private(pkey.clone()),
            algorithm: algorithm.clone(),
            extractable,
            key_type: "private".to_string(),
            usages: usages.clone(),
            public_key: None,
            private_key: None,
        };
        Ok(Self {
            material: KeyMaterial::Private(pkey),
            algorithm,
            extractable,
            key_type: "private".to_string(),
            usages,
            public_key: Some(Box::new(pub_key)),
            private_key: Some(Box::new(priv_key)),
        })
    }

    // --- accessors ---------------------------------------------------------

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

    /// Key-format identifiers, e.g. `CryptoKey.Format.JWK === "jwk"`. Pass one of
    /// these strings to `importKey` / `exportKey` / `wrapKey` / `unwrapKey`.
    #[napi(js_name = "Format", getter)]
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

    /// The public half of an asymmetric key pair, else `undefined`.
    #[napi(getter, js_name = "publicKey")]
    pub fn get_public_key_prop(&self) -> Option<CryptoKey> {
        self.public_key.as_deref().cloned()
    }

    /// The private half of an asymmetric key pair, else `undefined`.
    #[napi(getter, js_name = "privateKey")]
    pub fn get_private_key_prop(&self) -> Option<CryptoKey> {
        self.private_key.as_deref().cloned()
    }

    fn check_usage(&self, usage: &str) -> Result<()> {
        if self.usages.iter().any(|u| u == usage) {
            Ok(())
        } else {
            Err(bad(format!(
                "The requested operation is not valid for this key; '{usage}' is not in its usages"
            )))
        }
    }

    // --- key management (static) -------------------------------------------

    /// Import key material in `format` (`raw`, `raw-secret`, `pkcs8`, `spki`,
    /// or `jwk`).
    #[napi]
    pub fn import_key(
        format: String,
        key_data: Either<Uint8Array, Buffer>,
        algorithm: Either<String, CryptoKeyAlgorithm>,
        extractable: bool,
        usages: Vec<String>,
    ) -> Result<CryptoKey> {
        let mut alg = to_key_algorithm(algorithm);
        let kind = alg_kind(&alg.name)
            .ok_or_else(|| bad(format!("unsupported algorithm: {}", alg.name)))?;
        validate_usages(kind, &alg.name, &usages)?;

        let (material, key_type): (KeyMaterial, &str) = match format.as_str() {
            "raw" | "raw-secret" => {
                let bytes = match &key_data {
                    Either::A(b) => b.as_ref().to_vec(),
                    Either::B(_) => return Err(bad("raw key material must be bytes")),
                };
                match kind {
                    AlgKind::Aes | AlgKind::ChaCha | AlgKind::Mac => {
                        (KeyMaterial::Secret(bytes), "secret")
                    }
                    AlgKind::Ed25519 => (
                        KeyMaterial::Private(PKey::private_key_from_raw_bytes(
                            &bytes,
                            Id::ED25519,
                        )?),
                        "private",
                    ),
                    AlgKind::X25519 => (
                        KeyMaterial::Private(PKey::private_key_from_raw_bytes(&bytes, Id::X25519)?),
                        "private",
                    ),
                    AlgKind::Ecdsa | AlgKind::Ecdh => {
                        let nid = curve_nid(alg.named_curve.as_deref().unwrap_or("P-256"))?;
                        let group = EcGroup::from_curve_name(nid)?;
                        let ctx = BigNumContext::new()?;
                        let d = BigNum::from_slice(&bytes)?;
                        let point = EcPoint::mul_generator(&group, &d, &ctx)?;
                        let ec = EcKey::from_private_components(&group, &d, Some(&point))?;
                        (KeyMaterial::Private(PKey::from_ec_key(ec)?), "private")
                    }
                    _ => return Err(bad(format!("{} cannot be imported as raw", alg.name))),
                }
            }
            "pkcs8" => {
                let bytes = match &key_data {
                    Either::A(b) => b.as_ref().to_vec(),
                    Either::B(_) => return Err(bad("pkcs8 key material must be bytes")),
                };
                (
                    KeyMaterial::Private(PKey::private_key_from_pkcs8(&bytes)?),
                    "private",
                )
            }
            "spki" => {
                let bytes = match &key_data {
                    Either::A(b) => b.as_ref().to_vec(),
                    Either::B(_) => return Err(bad("spki key material must be bytes")),
                };
                (KeyMaterial::Public(spki_decode(&bytes)?), "public")
            }
            "jwk" => match &key_data {
                Either::B(v) => return jwk_decode(v, alg, extractable, usages),
                Either::A(_) => return Err(bad("jwk key material must be a JWK object")),
            },
            other => return Err(bad(format!("unsupported key format: {other}"))),
        };

        fill_derived(&mut alg, &material);
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

    /// Export a key. `jwk` returns the JWK object; every other format returns a
    /// `Buffer` of DER/raw bytes.
    #[napi]
    pub fn export_key(
        format: String,
        key: &CryptoKey,
    ) -> Result<Either<Buffer, serde_json::Value>> {
        if !key.extractable {
            return Err(bad("The key is not extractable"));
        }
        match format.as_str() {
            "jwk" => Ok(Either::B(jwk_encode(key)?)),
            "raw" | "raw-secret" => {
                let bytes = match &key.material {
                    KeyMaterial::Secret(b) => b.clone(),
                    KeyMaterial::Public(p) if p.id() == Id::ED25519 || p.id() == Id::X25519 => {
                        p.raw_public_key()?
                    }
                    KeyMaterial::Private(p) if p.id() == Id::ED25519 || p.id() == Id::X25519 => {
                        p.raw_private_key()?
                    }
                    _ => {
                        return Err(bad(format!(
                            "{} cannot be exported as raw",
                            key.algorithm.name
                        )));
                    }
                };
                Ok(Either::A(Buffer::from(bytes)))
            }
            "pkcs8" => match &key.material {
                KeyMaterial::Private(p) => Ok(Either::A(Buffer::from(p.private_key_to_pkcs8()?))),
                _ => Err(bad("only private keys can be exported as pkcs8")),
            },
            "spki" => {
                let public = to_public_of(key)?;
                Ok(Either::A(Buffer::from(spki_encode(&public)?)))
            }
            other => Err(bad(format!("unsupported key format: {other}"))),
        }
    }

    /// Extract the public half of an asymmetric key.
    #[napi]
    pub fn get_public_key(key: &CryptoKey) -> Result<CryptoKey> {
        match &key.material {
            KeyMaterial::Public(_) => Ok(CryptoKey {
                material: key.material.clone(),
                algorithm: key.algorithm.clone(),
                extractable: key.extractable,
                key_type: "public".to_string(),
                usages: key.usages.clone(),
                public_key: None,
                private_key: None,
            }),
            KeyMaterial::Private(p) => {
                let public = public_from_private(p)?;
                let mut alg = key.algorithm.clone();
                fill_derived(&mut alg, &KeyMaterial::Public(public.clone()));
                Ok(CryptoKey {
                    material: KeyMaterial::Public(public),
                    algorithm: alg,
                    extractable: key.extractable,
                    key_type: "public".to_string(),
                    usages: key.usages.clone(),
                    public_key: None,
                    private_key: None,
                })
            }
            KeyMaterial::Secret(_) => Err(bad("symmetric keys have no public half")),
        }
    }

    /// Feature detection: is `operation` supported for `algorithm`?
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

    // --- operations (static) ----------------------------------------------

    #[napi(js_name = "encrypt")]
    pub fn encrypt_static(
        algorithm: CryptoOpParams,
        key: &CryptoKey,
        data: Uint8Array,
    ) -> Result<Buffer> {
        Ok(Buffer::from(encrypt_impl(&algorithm, key, data.as_ref())?))
    }

    #[napi(js_name = "decrypt")]
    pub fn decrypt_static(
        algorithm: CryptoOpParams,
        key: &CryptoKey,
        data: Uint8Array,
    ) -> Result<Buffer> {
        Ok(Buffer::from(decrypt_impl(&algorithm, key, data.as_ref())?))
    }

    #[napi(js_name = "sign")]
    pub fn sign_static(
        algorithm: CryptoOpParams,
        key: &CryptoKey,
        data: Uint8Array,
    ) -> Result<Buffer> {
        key.check_usage("sign")?;
        let name = algorithm
            .name
            .clone()
            .ok_or_else(|| bad("sign requires algorithm.name"))?;
        let kind = alg_kind(&name).ok_or_else(|| bad(format!("unsupported algorithm: {name}")))?;
        let out = match kind {
            AlgKind::Mac => {
                let k = to_secret_of(key)?;
                let pkey = PKey::hmac(&k)?;
                let mut s = Signer::new(digest_from_name(&hash_name(&algorithm.hash))?, &pkey)?;
                s.update(data.as_ref())?;
                s.sign_to_vec()?
            }
            AlgKind::Ecdsa | AlgKind::Ed25519 | AlgKind::RsaPss | AlgKind::RsaPkcs1 => {
                let p = to_private_of(key)?;
                sign_impl(
                    kind,
                    &p,
                    &hash_name(&algorithm.hash),
                    algorithm.salt_length,
                    data.as_ref(),
                )?
            }
            _ => return Err(bad(format!("{name} does not support signing"))),
        };
        Ok(Buffer::from(out))
    }

    #[napi(js_name = "verify")]
    pub fn verify_static(
        algorithm: CryptoOpParams,
        key: &CryptoKey,
        signature: Uint8Array,
        data: Uint8Array,
    ) -> Result<bool> {
        key.check_usage("verify")?;
        let name = algorithm
            .name
            .clone()
            .ok_or_else(|| bad("verify requires algorithm.name"))?;
        let kind = alg_kind(&name).ok_or_else(|| bad(format!("unsupported algorithm: {name}")))?;
        match kind {
            AlgKind::Mac => {
                let k = to_secret_of(key)?;
                let pkey = PKey::hmac(&k)?;
                let mut v = Verifier::new(digest_from_name(&hash_name(&algorithm.hash))?, &pkey)?;
                v.update(data.as_ref())?;
                Ok(v.verify(signature.as_ref())?)
            }
            AlgKind::Ecdsa | AlgKind::Ed25519 | AlgKind::RsaPss | AlgKind::RsaPkcs1 => {
                let p = to_public_of(key)?;
                verify_impl(
                    kind,
                    &p,
                    &hash_name(&algorithm.hash),
                    data.as_ref(),
                    signature.as_ref(),
                )
            }
            _ => Err(bad(format!("{name} does not support verification"))),
        }
    }

    #[napi(js_name = "deriveBits")]
    pub fn derive_bits_static(
        algorithm: CryptoOpParams,
        base_key: &CryptoKey,
        length: Option<u32>,
    ) -> Result<Buffer> {
        Ok(Buffer::from(derive_bits_impl(
            &algorithm, base_key, length,
        )?))
    }

    /// Derive a key: `deriveBits` followed by `importKey` into `derivedKeyType`.
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
        let raw = derive_bits_impl(&algorithm, base_key, Some(bits))?;
        Self::import_key(
            "raw".to_string(),
            Either::A(Uint8Array::new(raw)),
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
        let md = digest_from_name(&name)?;
        Ok(Buffer::from(hash(md, data.as_ref())?.to_vec()))
    }

    /// Wrap `key` with `wrappingKey` using `algorithm`.
    #[napi(js_name = "wrapKey")]
    pub fn wrap_key_static(
        format: String,
        key: &CryptoKey,
        wrapping_key: &CryptoKey,
        algorithm: Either<String, CryptoOpParams>,
    ) -> Result<Buffer> {
        let (name, params) = match algorithm {
            Either::A(s) => (
                s.clone(),
                CryptoOpParams {
                    name: Some(s),
                    iv: None,
                    additional_data: None,
                    tag_length: None,
                    iterations: None,
                    salt: None,
                    info: None,
                    label: None,
                    hash: None,
                    salt_length: None,
                    public: None,
                    length: None,
                },
            ),
            Either::B(p) => (p.name.clone().unwrap_or_default(), p),
        };
        wrapping_key.check_usage("wrapKey")?;
        let exported = match Self::export_key(format, key)? {
            Either::A(b) => b.to_vec(),
            Either::B(v) => {
                serde_json::to_vec(&v).map_err(|e| bad(format!("cannot serialize JWK: {e}")))?
            }
        };
        let kind = alg_kind(&name).ok_or_else(|| bad(format!("unsupported algorithm: {name}")))?;
        let iv = params.iv.as_ref().map(|u| u.as_ref().to_vec());
        let aad = params
            .additional_data
            .as_ref()
            .map(|u| u.as_ref().to_vec())
            .unwrap_or_default();
        match (&wrapping_key.material, kind) {
            (KeyMaterial::Secret(k), AlgKind::Aes) if name == "AES-KW" => {
                Ok(Buffer::from(aes_kw_wrap(k, &exported)?))
            }
            (KeyMaterial::Secret(k), AlgKind::Aes) if is_aead(&name) => {
                let iv = iv.ok_or_else(|| bad(format!("{name} requires an iv")))?;
                let bits = (k.len() * 8) as u32;
                Ok(Buffer::from(aead_seal(
                    aes_cipher(&name, bits)?,
                    k,
                    &iv,
                    &aad,
                    &exported,
                )?))
            }
            (KeyMaterial::Secret(k), AlgKind::Aes) => {
                let iv = iv.ok_or_else(|| bad(format!("{name} requires an iv")))?;
                let bits = (k.len() * 8) as u32;
                Ok(Buffer::from(block_crypt(
                    aes_cipher(&name, bits)?,
                    Mode::Encrypt,
                    k,
                    &iv,
                    &exported,
                )?))
            }
            (KeyMaterial::Secret(k), AlgKind::ChaCha) => {
                let iv = iv.ok_or_else(|| bad("ChaCha20-Poly1305 requires an iv"))?;
                Ok(Buffer::from(aead_seal(
                    Cipher::chacha20_poly1305(),
                    k,
                    &iv,
                    &aad,
                    &exported,
                )?))
            }
            (KeyMaterial::Public(p), AlgKind::RsaOaep) => {
                let md = digest_from_name(&hash_name(&params.hash))?;
                let mut e = Encrypter::new(p)?;
                e.set_rsa_padding(Padding::PKCS1_OAEP)?;
                e.set_rsa_oaep_md(md)?;
                Ok(Buffer::from(e.encrypt(&exported)?))
            }
            _ => Err(bad(format!("{name} cannot be used as a wrapping key"))),
        }
    }

    /// Unwrap a key produced by `wrapKey`.
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
        unwrapping_key.check_usage("unwrapKey")?;
        let (name, params) = match wrap_algorithm {
            Either::A(s) => (
                s.clone(),
                CryptoOpParams {
                    name: Some(s),
                    iv: None,
                    additional_data: None,
                    tag_length: None,
                    iterations: None,
                    salt: None,
                    info: None,
                    label: None,
                    hash: None,
                    salt_length: None,
                    public: None,
                    length: None,
                },
            ),
            Either::B(p) => (p.name.clone().unwrap_or_default(), p),
        };
        let data = wrapped_key.as_ref();
        let kind = alg_kind(&name).ok_or_else(|| bad(format!("unsupported algorithm: {name}")))?;
        let iv = params.iv.as_ref().map(|u| u.as_ref().to_vec());
        let aad = params
            .additional_data
            .as_ref()
            .map(|u| u.as_ref().to_vec())
            .unwrap_or_default();
        let plaintext = match (&unwrapping_key.material, kind) {
            (KeyMaterial::Secret(k), AlgKind::Aes) if name == "AES-KW" => aes_kw_unwrap(k, data)?,
            (KeyMaterial::Secret(k), AlgKind::Aes) if is_aead(&name) => {
                let iv = iv.ok_or_else(|| bad(format!("{name} requires an iv")))?;
                let bits = (k.len() * 8) as u32;
                aead_open(aes_cipher(&name, bits)?, k, &iv, &aad, data)?
            }
            (KeyMaterial::Secret(k), AlgKind::Aes) => {
                let iv = iv.ok_or_else(|| bad(format!("{name} requires an iv")))?;
                let bits = (k.len() * 8) as u32;
                block_crypt(aes_cipher(&name, bits)?, Mode::Decrypt, k, &iv, data)?
            }
            (KeyMaterial::Secret(k), AlgKind::ChaCha) => {
                let iv = iv.ok_or_else(|| bad("ChaCha20-Poly1305 requires an iv"))?;
                aead_open(Cipher::chacha20_poly1305(), k, &iv, &aad, data)?
            }
            (KeyMaterial::Private(p), AlgKind::RsaOaep) => {
                let md = digest_from_name(&hash_name(&params.hash))?;
                let mut d = Decrypter::new(p)?;
                d.set_rsa_padding(Padding::PKCS1_OAEP)?;
                d.set_rsa_oaep_md(md)?;
                d.decrypt(data)?
            }
            _ => return Err(bad(format!("{name} cannot be used as an unwrapping key"))),
        };

        if format == "jwk" {
            let v: serde_json::Value = serde_json::from_slice(&plaintext)
                .map_err(|e| bad(format!("unwrapped JWK is not valid JSON: {e}")))?;
            return jwk_decode(&v, derived_key_algorithm, extractable, key_usages);
        }
        Self::import_key(
            format,
            Either::A(Uint8Array::new(plaintext)),
            Either::B(derived_key_algorithm),
            extractable,
            key_usages,
        )
    }

    // --- operations (instance) --------------------------------------------

    #[napi]
    pub fn encrypt(
        &self,
        algorithm: CryptoOpParams,
        key: &CryptoKey,
        data: Uint8Array,
    ) -> Result<Buffer> {
        Self::encrypt_static(algorithm, key, data)
    }

    #[napi]
    pub fn decrypt(
        &self,
        algorithm: CryptoOpParams,
        key: &CryptoKey,
        data: Uint8Array,
    ) -> Result<Buffer> {
        Self::decrypt_static(algorithm, key, data)
    }

    #[napi]
    pub fn sign(
        &self,
        algorithm: CryptoOpParams,
        key: &CryptoKey,
        data: Uint8Array,
    ) -> Result<Buffer> {
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
    pub fn export_key_instance(&self, format: String) -> Result<Either<Buffer, serde_json::Value>> {
        Self::export_key(format, self)
    }

    #[napi]
    pub fn get_public_key_instance(&self) -> Result<CryptoKey> {
        Self::get_public_key(self)
    }

    /// Derive a key from `base_key` (instance form of `CryptoKey.deriveKey`).
    #[napi]
    pub fn derive_key(
        &self,
        algorithm: CryptoOpParams,
        base_key: &CryptoKey,
        derived_key_type: CryptoOpParams,
        extractable: bool,
        key_usages: Vec<String>,
    ) -> Result<CryptoKey> {
        Self::derive_key_static(
            algorithm,
            base_key,
            derived_key_type,
            extractable,
            key_usages,
        )
    }

    /// Wrap `key` (instance form of `CryptoKey.wrapKey`).
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

    /// Unwrap a key (instance form of `CryptoKey.unwrapKey`).
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

    /// Digest `data` (instance form of `CryptoKey.digest`).
    #[napi]
    pub fn digest(&self, algorithm: CryptoOpParams, data: Uint8Array) -> Result<Buffer> {
        Self::digest_static(algorithm, data)
    }

    // Post-quantum KEMs are out of scope for this build; the surface is present
    // so callers can feature-detect and fail with a clear error.

    /// ML-KEM-based key encapsulation. Not implemented in this build.
    #[napi(js_name = "encapsulateBits")]
    pub fn encapsulate_bits_static(
        _algorithm: CryptoOpParams,
        _public_key: &CryptoKey,
    ) -> Result<Buffer> {
        Err(bad(
            "encapsulateBits (ML-KEM) is not implemented in this build",
        ))
    }

    /// ML-KEM-based key decapsulation. Not implemented in this build.
    #[napi(js_name = "decapsulateBits")]
    pub fn decapsulate_bits_static(
        _algorithm: CryptoOpParams,
        _private_key: &CryptoKey,
        _ciphertext: Uint8Array,
    ) -> Result<Buffer> {
        Err(bad(
            "decapsulateBits (ML-KEM) is not implemented in this build",
        ))
    }

    /// ML-KEM-based key encapsulation into a `CryptoKey`. Not implemented.
    #[napi(js_name = "encapsulateKey")]
    pub fn encapsulate_key_static(
        _algorithm: CryptoOpParams,
        _public_key: &CryptoKey,
        _derived_key_type: CryptoOpParams,
        _extractable: bool,
        _key_usages: Vec<String>,
    ) -> Result<CryptoKey> {
        Err(bad(
            "encapsulateKey (ML-KEM) is not implemented in this build",
        ))
    }

    /// ML-KEM-based key decapsulation into a `CryptoKey`. Not implemented.
    #[napi(js_name = "decapsulateKey")]
    pub fn decapsulate_key_static(
        _algorithm: CryptoOpParams,
        _private_key: &CryptoKey,
        _ciphertext: Uint8Array,
        _derived_key_type: CryptoOpParams,
        _extractable: bool,
        _key_usages: Vec<String>,
    ) -> Result<CryptoKey> {
        Err(bad(
            "decapsulateKey (ML-KEM) is not implemented in this build",
        ))
    }
    /// ML-KEM-based key encapsulation (instance form of `CryptoKey.encapsulateBits`).
    #[napi(js_name = "encapsulateBits")]
    pub fn encapsulate_bits(
        &self,
        algorithm: CryptoOpParams,
        public_key: &CryptoKey,
    ) -> Result<Buffer> {
        Self::encapsulate_bits_static(algorithm, public_key)
    }

    /// ML-KEM-based key decapsulation (instance form of `CryptoKey.decapsulateBits`).
    #[napi(js_name = "decapsulateBits")]
    pub fn decapsulate_bits(
        &self,
        algorithm: CryptoOpParams,
        private_key: &CryptoKey,
        ciphertext: Uint8Array,
    ) -> Result<Buffer> {
        Self::decapsulate_bits_static(algorithm, private_key, ciphertext)
    }

    /// ML-KEM-based key encapsulation into a `CryptoKey` (instance form of
    /// `CryptoKey.encapsulateKey`).
    #[napi(js_name = "encapsulateKey")]
    pub fn encapsulate_key(
        &self,
        algorithm: CryptoOpParams,
        public_key: &CryptoKey,
        derived_key_type: CryptoOpParams,
        extractable: bool,
        key_usages: Vec<String>,
    ) -> Result<CryptoKey> {
        Self::encapsulate_key_static(
            algorithm,
            public_key,
            derived_key_type,
            extractable,
            key_usages,
        )
    }

    /// ML-KEM-based key decapsulation into a `CryptoKey` (instance form of
    /// `CryptoKey.decapsulateKey`).
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

// ---------------------------------------------------------------------------
// Tests

/// A format-tagged cryptographic signature value.
///
/// Construct it with the key format the signature corresponds to, then load the
/// raw signature bytes:
///
/// ```ts
/// const sig = new Signature(CryptoKey.Format.RAW); // "raw"
/// sig.setBytes(signatureBytes);
/// ```
///
/// `format` is any value from the `CryptoKey.Format` table (e.g. `"raw"`,
/// `"jwk"`, `"spki"`, `"pkcs8"`, `"raw-secret"`, `"raw-public"`, `"raw-seed"`).
#[napi]
pub struct Signature {
    format: String,
    bytes: Vec<u8>,
}

#[napi]
impl Signature {
    /// Create an empty signature tagged with `format`.
    #[napi(constructor)]
    pub fn new(format: String) -> Self {
        Self {
            format,
            bytes: Vec::new(),
        }
    }

    /// The key format this signature is associated with.
    #[napi(getter)]
    pub fn format(&self) -> String {
        self.format.clone()
    }

    /// Replace the signature bytes.
    #[napi]
    pub fn set_bytes(&mut self, bytes: Buffer) {
        self.bytes = bytes.as_ref().to_vec();
    }

    /// The raw signature bytes.
    #[napi(getter)]
    pub fn bytes(&self) -> Buffer {
        Buffer::from(self.bytes.clone())
    }

    /// Length in bytes.
    #[napi(getter)]
    pub fn length(&self) -> u32 {
        self.bytes.len() as u32
    }

    /// Whether the signature is empty.
    #[napi(getter)]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Lowercase hex form.
    #[napi(getter)]
    pub fn hex(&self) -> String {
        hex_lower(&self.bytes)
    }
}
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn deterministic_key(byte: u8) -> CryptoKey {
        CryptoKey::from_bytes(&vec![byte; KEY_SIZE]).unwrap()
    }

    // ---- CryptoKey ----

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = SymmetricCryptoKey::generate_random();
        let plaintext = b"Hello, World!";
        let ciphertext = key.encrypt(plaintext).unwrap();
        assert_ne!(plaintext.as_slice(), &ciphertext[..]);
        let decrypted = key.decrypt(&ciphertext).unwrap();
        assert_eq!(plaintext.as_slice(), &decrypted[..]);
    }

    #[test]
    fn encrypt_is_randomized_and_detected() {
        let key = SymmetricCryptoKey::generate_random();
        let c1 = key.encrypt(b"same").unwrap();
        let c2 = key.encrypt(b"same").unwrap();
        assert_ne!(c1, c2, "fresh nonce per call");
        assert!(SymmetricCryptoKey::is_encrypted(&c1));
        assert!(!SymmetricCryptoKey::is_encrypted(b"plain"));
    }

    #[test]
    fn wrong_key_fails_and_tamper_detected() {
        let key1 = SymmetricCryptoKey::generate_random();
        let key2 = SymmetricCryptoKey::generate_random();
        let ct = key1.encrypt(b"secret").unwrap();
        assert!(key2.decrypt(&ct).is_err());

        let mut tampered = ct.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xFF;
        assert!(key1.decrypt(&tampered).is_err());
    }

    #[test]
    fn empty_and_invalid_format() {
        let key = SymmetricCryptoKey::generate_random();
        let ct = key.encrypt(b"").unwrap();
        assert_eq!(key.decrypt(&ct).unwrap(), b"".to_vec());
        assert!(key.decrypt(&[1, 2, 3]).is_err());
        assert!(SymmetricCryptoKey::from_bytes(&[0u8; KEY_SIZE - 1]).is_err());
    }

    // ---- KeyManager ----

    #[test]
    fn init_generate_and_load() {
        let temp = TempDir::new().unwrap();
        let km = KeyManager::new(temp.path());
        assert!(!km.is_initialized());

        km.init_dirs().unwrap();
        assert!(km.is_initialized());
        assert!(km.init_dirs().is_err(), "double init fails");

        let key = km.generate_key().unwrap();
        let loaded = km.load_key().unwrap();
        assert_eq!(key.as_bytes(), loaded.as_bytes());
        assert_eq!(
            km.default_key_path(),
            temp.path().join("crypt").join("keys").join("default")
        );
    }

    #[test]
    fn export_import_roundtrip() {
        let temp = TempDir::new().unwrap();
        let km = KeyManager::new(temp.path());
        km.init_dirs().unwrap();
        let key = km.generate_key().unwrap();

        let export_path = temp.path().join("exported.key");
        km.export_key(&export_path).unwrap();

        let temp2 = TempDir::new().unwrap();
        let km2 = KeyManager::new(temp2.path());
        km2.init_dirs().unwrap();
        km2.import_key(&export_path).unwrap();
        assert_eq!(km2.load_key().unwrap().as_bytes(), key.as_bytes());
    }

    #[test]
    fn load_before_init_fails() {
        let temp = TempDir::new().unwrap();
        let km = KeyManager::new(temp.path());
        assert!(km.load_key().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn key_permissions_are_0600() {
        use std::os::unix::fs::PermissionsExt;
        let temp = TempDir::new().unwrap();
        let km = KeyManager::new(temp.path());
        km.init_dirs().unwrap();
        km.generate_key().unwrap();
        let mode = std::fs::metadata(km.default_key_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // ---- recipient-name helpers (ssh feature) ----

    #[cfg(feature = "ssh")]
    #[test]
    fn sanitize_label_strips_invalid_chars() {
        assert_eq!(sanitize_label("alice@example.com"), "aliceexample.com");
        assert_eq!(sanitize_label("Team-Name_01"), "Team-Name_01");
        assert_eq!(sanitize_label("weird label!*"), "weirdlabel");
    }

    #[cfg(feature = "ssh")]
    #[test]
    fn derive_recipient_prefers_comment_field() {
        let pubkey = "ssh-ed25519 AAAAC3 comment@example.com";
        let name = derive_recipient_name(pubkey, Path::new("id_ed25519.pub"));
        assert_eq!(name, "commentexample.com");
    }

    #[cfg(feature = "ssh")]
    #[test]
    fn derive_recipient_falls_back_to_filename() {
        let pubkey = "ssh-ed25519 AAAAC3";
        let name = derive_recipient_name(pubkey, Path::new("keys/test-id.pub"));
        assert_eq!(name, "test-id");
    }

    #[cfg(feature = "ssh")]
    #[test]
    fn fallback_fingerprint_is_deterministic() {
        let fp1 = fallback_fingerprint("ssh-ed25519 AAAAC3");
        let fp2 = fallback_fingerprint("ssh-ed25519 AAAAC3");
        assert_eq!(fp1, fp2);
        assert!(fp1.starts_with("ssh-"));
    }

    #[cfg(feature = "ssh")]
    #[test]
    fn ring_manager_ssh_roundtrip() {
        use ring::signature::{Ed25519KeyPair, KeyPair as _};

        // Generate a valid ed25519 keypair from a random seed.
        let mut seed = [0u8; 32];
        SystemRandom::new().fill(&mut seed).unwrap();
        let kp = Ed25519KeyPair::from_seed_unchecked(&seed).unwrap();
        let public: [u8; 32] = kp.public_key().as_ref().try_into().unwrap();

        // Build the ssh-ed25519 public-key string.
        let mut pubblob = Vec::new();
        ssh_string(&mut pubblob, b"ssh-ed25519");
        ssh_string(&mut pubblob, &public);
        let pub_string = format!("ssh-ed25519 {}", b64_encode(&pubblob));

        // Build an unencrypted OpenSSH private-key PEM for the same keypair.
        let pem = build_openssh_ed25519_pem(&seed, &public);

        let repo_key = CryptoKey::generate();
        let enc = IdentityManager::encrypt_key_for_ssh_recipient(&repo_key, &pub_string).unwrap();
        let dec = IdentityManager::decrypt_key_with_ssh_identity(&enc, &pem, None).unwrap();
        assert_eq!(dec.as_bytes(), repo_key.as_bytes());
    }

    #[cfg(feature = "ssh")]
    #[test]
    fn ssh_identity_sign_verify() {
        use ring::signature::{Ed25519KeyPair, KeyPair as _};
        let mut seed = [0u8; 32];
        SystemRandom::new().fill(&mut seed).unwrap();
        let kp = Ed25519KeyPair::from_seed_unchecked(&seed).unwrap();
        let public: [u8; 32] = kp.public_key().as_ref().try_into().unwrap();
        let pem = build_openssh_ed25519_pem(&seed, &public);

        let id = SshIdentity::from_private_key(pem, None).unwrap();
        let data = b"sign me".to_vec();
        // Sign twice: Ed25519 signatures are deterministic for a given key+message, so both
        // signatures are identical and each `verify` call can take ownership of its own.
        let sig = id.sign(Buffer::from(data.clone())).unwrap();
        assert!(id.verify(Buffer::from(data.clone()), sig).unwrap());
        let sig2 = id.sign(Buffer::from(data.clone())).unwrap();
        assert!(!id.verify(Buffer::from(b"other".to_vec()), sig2).unwrap());
    }

    #[cfg(feature = "ssh")]
    fn ssh_string(out: &mut Vec<u8>, s: &[u8]) {
        out.extend_from_slice(&(s.len() as u32).to_be_bytes());
        out.extend_from_slice(s);
    }

    #[cfg(feature = "ssh")]
    fn b64_encode(data: &[u8]) -> String {
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b0 = chunk[0] as usize;
            let b1 = if chunk.len() > 1 {
                chunk[1] as usize
            } else {
                0
            };
            let b2 = if chunk.len() > 2 {
                chunk[2] as usize
            } else {
                0
            };
            let n = (b0 << 16) | (b1 << 8) | b2;
            out.push(A[(n >> 18) & 63] as char);
            out.push(A[(n >> 12) & 63] as char);
            out.push(if chunk.len() > 1 {
                A[(n >> 6) & 63] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                A[n & 63] as char
            } else {
                '='
            });
        }
        out
    }

    #[cfg(feature = "ssh")]
    fn build_openssh_ed25519_pem(seed: &[u8; 32], public: &[u8; 32]) -> String {
        let mut body = Vec::new();
        body.extend_from_slice(b"openssh-key-v1\0");
        ssh_string(&mut body, b"none");
        ssh_string(&mut body, b"none");
        ssh_string(&mut body, b"");
        body.extend_from_slice(&1u32.to_be_bytes());

        let mut pubblob = Vec::new();
        ssh_string(&mut pubblob, b"ssh-ed25519");
        ssh_string(&mut pubblob, public);
        ssh_string(&mut body, &pubblob);

        let mut privsec = Vec::new();
        let check: u32 = 0x1234_5678;
        privsec.extend_from_slice(&check.to_be_bytes());
        privsec.extend_from_slice(&check.to_be_bytes());
        ssh_string(&mut privsec, b"ssh-ed25519");
        ssh_string(&mut privsec, public);
        let mut privseed = Vec::new();
        privseed.extend_from_slice(seed);
        privseed.extend_from_slice(public);
        ssh_string(&mut privsec, &privseed);
        ssh_string(&mut privsec, b""); // comment
        let padlen = 8 - (privsec.len() % 8);
        for i in 1..=padlen {
            privsec.push(i as u8);
        }
        ssh_string(&mut body, &privsec);

        let b64 = b64_encode(&body);
        format!("-----BEGIN OPENSSH PRIVATE KEY-----\n{b64}\n-----END OPENSSH PRIVATE KEY-----")
    }
}
