//! Transparent file encryption ("crypt") layered on top of the gix repository.
//!
//! This module ports a standalone git-crypt implementation into a single file and
//! exposes it through a [`Crypt`] NAPI handle obtained from
//! [`crate::repository::Repository::crypt`].
//!
//! Design:
//! - A per-repository symmetric key lives at `<git-dir>/crypt/keys/default`, written
//!   with `0600` permissions on Unix and never committed (it stays inside `.git/`).
//! - Encryption uses AES-256-GCM (via the `ring` crate) with a fresh 96-bit random
//!   nonce per call. The wire format is `GITCRYPT` magic header + 12-byte nonce +
//!   ciphertext + 16-byte tag, so [`CryptoKey::is_encrypted`] can reliably detect
//!   already-encrypted blobs (idempotent clean/smudge).
//! - The JS surface uses buffer-based [`Crypt::encrypt`]/[`Crypt::decrypt`]. The
//!   stdin/stdout git filter drivers ([`clean_filter`]/[`smudge_filter`]/[`diff_filter`])
//!   are kept as internal free functions for a future CLI binary and are NOT exposed as
//!   NAPI calls (they cannot run inside a Node process).
//! - `init`/`unlock` still write `filter.crypt.{clean,smudge,diff,required}` into the local
//!   git config so git-CLI interop keeps working; `lock` removes them.
//! - Recipient management (`addSshUser`/`importSshKey`/`addGpgUser`) is repo-agnostic:
//!   those are static methods that open the repository themselves. SSH recipients use
//!   ed25519 keys only (curve25519-dalek for X25519 + ssh-key for parsing); there is no
//!   `age`/`rage` dependency.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use napi::bindgen_prelude::Buffer;
use napi::Result;
use napi_derive::napi;
use chacha20poly1305::ChaCha20Poly1305;
use chacha20poly1305::aead::{Aead, KeyInit, Key, Nonce};
use napi::bindgen_prelude::{Either, Either6};
use rand::Rng;
use multi_sig::prelude::*;
use multi_util::CodecInfo as _;
use thiserror::Error;

use crate::error::thread_local;

/// Domain errors for the crypt subsystem. Converted into [`napi::Error`] at the NAPI
/// boundary so internal helpers can stay typed and use `?`.
#[derive(Debug, Error)]
pub enum CryptError {
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
type CryptResult<T> = std::result::Result<T, CryptError>;

impl From<CryptError> for napi::Error {
    fn from(e: CryptError) -> Self {
        napi::Error::from_reason(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// CryptoKey (AES-256-GCM via ring)
// ---------------------------------------------------------------------------

/// Key size in bytes (256 bits).
pub const KEY_SIZE: usize = 32;
/// Nonce size in bytes (96 bits, ChaCha20-Poly1305 standard).
pub const NONCE_SIZE: usize = 12;
/// Magic header identifying crypt-encrypted payloads.
const MAGIC_HEADER: &[u8] = b"GITCRYPT";

/// A symmetric ChaCha20-Poly1305 key used internally by the repo crypt subsystem.
#[derive(Clone)]
pub struct CryptKey {
    key: [u8; KEY_SIZE],
}

impl CryptKey {
    /// Generate a new random key from the OS CSPRNG.
    pub fn generate_random() -> Self {
        let mut key = [0u8; KEY_SIZE];
        rand::rng().fill_bytes(&mut key);
        Self { key }
    }

    /// Alias for [`CryptKey::generate_random`].
    pub fn generate() -> Self {
        Self::generate_random()
    }

    /// Build a key from raw bytes, rejecting any length other than [`KEY_SIZE`].
    pub fn from_bytes(bytes: &[u8]) -> CryptResult<Self> {
        if bytes.len() != KEY_SIZE {
            return Err(CryptError::InvalidKeyFormat);
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
    pub fn encrypt(&self, plaintext: &[u8]) -> CryptResult<Vec<u8>> {
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        rand::rng().fill_bytes(&mut nonce_bytes);

        let cipher = ChaCha20Poly1305::new(Key::<ChaCha20Poly1305>::from_slice(&self.key));
        let ciphertext = cipher
            .encrypt(Nonce::<ChaCha20Poly1305>::from_slice(&nonce_bytes), plaintext)
            .map_err(|_| CryptError::Crypto("encryption failed".into()))?;

        let mut result = Vec::with_capacity(MAGIC_HEADER.len() + NONCE_SIZE + ciphertext.len());
        result.extend_from_slice(MAGIC_HEADER);
        result.extend_from_slice(&nonce_bytes);
        result.extend_from_slice(&ciphertext);
        Ok(result)
    }

    /// Decrypt a payload produced by [`CryptKey::encrypt`].
    pub fn decrypt(&self, ciphertext: &[u8]) -> CryptResult<Vec<u8>> {
        let min_size = MAGIC_HEADER.len() + NONCE_SIZE;
        if ciphertext.len() < min_size {
            return Err(CryptError::Crypto("ciphertext too short".into()));
        }
        if &ciphertext[..MAGIC_HEADER.len()] != MAGIC_HEADER {
            return Err(CryptError::Crypto("invalid encrypted data format".into()));
        }

        let data = &ciphertext[MAGIC_HEADER.len()..];
        let (nonce_bytes, enc) = data.split_at(NONCE_SIZE);

        let cipher = ChaCha20Poly1305::new(Key::<ChaCha20Poly1305>::from_slice(&self.key));
        cipher
            .decrypt(Nonce::<ChaCha20Poly1305>::from_slice(nonce_bytes), enc)
            .map_err(|_| CryptError::Crypto("decryption failed (wrong key or tampered)".into()))
    }

    /// Whether `data` carries the crypt magic header.
    pub fn is_encrypted(data: &[u8]) -> bool {
        data.len() >= MAGIC_HEADER.len() && &data[..MAGIC_HEADER.len()] == MAGIC_HEADER
    }
}

// ---------------------------------------------------------------------------
// KeyManager (on-disk key storage under <git-dir>/crypt)
// ---------------------------------------------------------------------------

/// Stores and manages the repository's crypt key on disk.
pub struct KeyManager {
    git_dir: PathBuf,
}

impl KeyManager {
    /// Create a manager rooted at `git_dir` (the `.git` directory).
    pub fn new(git_dir: impl AsRef<Path>) -> Self {
        Self {
            git_dir: git_dir.as_ref().to_path_buf(),
        }
    }

    /// Path to the `<git-dir>/crypt` directory.
    pub fn git_crypt_dir(&self) -> PathBuf {
        self.git_dir.join("crypt")
    }

    /// Path to the default symmetric key file.
    pub fn default_key_path(&self) -> PathBuf {
        self.git_crypt_dir().join("keys").join("default")
    }

    /// Create the `crypt` + `crypt/keys` directories. Fails if already initialized.
    pub fn init_dirs(&self) -> CryptResult<()> {
        let crypt_dir = self.git_crypt_dir();
        if crypt_dir.exists() {
            return Err(CryptError::AlreadyInitialized);
        }
        std::fs::create_dir_all(&crypt_dir)?;
        std::fs::create_dir(crypt_dir.join("keys"))?;
        Ok(())
    }

    /// Whether the repository has been initialized for crypt.
    pub fn is_initialized(&self) -> bool {
        self.git_crypt_dir().exists()
    }

    /// Generate a new random key and persist it.
    pub fn generate_key(&self) -> CryptResult<CryptKey> {
        let key = CryptKey::generate();
        self.save_key(&key)?;
        Ok(key)
    }

    /// Persist `key` to the default path with `0600` permissions on Unix.
    pub fn save_key(&self, key: &CryptKey) -> CryptResult<()> {
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

    /// Load the default key from disk.
    pub fn load_key(&self) -> CryptResult<CryptKey> {
        let key_path = self.default_key_path();
        if !key_path.exists() {
            return Err(CryptError::KeyNotFound("default".into()));
        }
        let mut file = std::fs::File::open(&key_path)?;
        let mut key_bytes = Vec::new();
        file.read_to_end(&mut key_bytes)?;
        CryptKey::from_bytes(&key_bytes)
    }

    /// Export the default key to `output_path` (with `0600` on Unix).
    pub fn export_key(&self, output_path: &Path) -> CryptResult<()> {
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

    /// Import a key from `input_path` and save it as the default key.
    pub fn import_key(&self, input_path: &Path) -> CryptResult<()> {
        let mut file = std::fs::File::open(input_path)?;
        let mut key_bytes = Vec::new();
        file.read_to_end(&mut key_bytes)?;
        let key = CryptKey::from_bytes(&key_bytes)?;
        self.save_key(&key)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Crypt NAPI handle
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// multi-sig: self-describing signature bindings
//
// A `Multisig` is serialized as:
//   SIGIL (0x1239) | signature_codec | Varbytes(message) | Varuint(num_attributes) |
//   [ AttrId | Varbytes(value) ] * num_attributes
//
// A Multisig is *combined* when `message` is non-empty (the signed payload travels in-band);
// otherwise it is *detached* and the verifier must supply the message out-of-band.
//
// These wrap the `multi_sig` crate's `Builder`/`Multisig` types. The actual sign/verify
// operations live in the companion `multi_key` crate (SignView/VerifyView on a Multikey).
// ---------------------------------------------------------------------------

/// Lowercase hex encoding, without pulling in an extra dependency.
fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Raw signature bytes. Mirrors `multi_sig::types::SignatureBytes`.
#[napi]
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
            Either::A(name) => multi_sig::prelude::Codec::try_from(name.as_str())
                .map_err(|e| napi::Error::from_reason(format!("unknown codec '{name}': {e}")))?,
            Either::B(code) => multi_sig::prelude::Codec::try_from(code)
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
pub struct Multisig {
    inner: multi_sig::Multisig,
}

#[napi]
impl Multisig {
    /// Decode a Multisig from its binary wire form.
    #[napi(constructor)]
    pub fn new(bytes: Buffer) -> Result<Self> {
        multi_sig::Multisig::try_from(bytes.as_ref())
            .map(|inner| Self { inner })
            .map_err(|e| napi::Error::from_reason(format!("decode multisig: {e}")))
    }

    /// The binary wire form.
    #[napi(getter)]
    pub fn bytes(&self) -> Buffer {
        Buffer::from(Vec::from(self.inner.clone()))
    }

    /// The base-encoded string form (lowercase hex, with the multibase `f` prefix) — identical to
    /// what [`MultisigBuilder::try_build_encoded`] produces.
    #[napi(getter)]
    pub fn hex(&self) -> String {
        let encoded: BaseEncoded<multi_sig::Multisig> =
            BaseEncoded::from(self.inner.clone());
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

/// Resolve an `Either<String, u32>` codec argument into a `multi_sig::prelude::Codec`.
fn resolve_codec(codec: Either<String, u32>) -> Result<multi_sig::prelude::Codec> {
    match codec {
        Either::A(name) => multi_sig::prelude::Codec::try_from(name.as_str())
            .map_err(|e| napi::Error::from_reason(format!("unknown codec '{name}': {e}"))),
        Either::B(code) => multi_sig::prelude::Codec::try_from(code)
            .map_err(|e| napi::Error::from_reason(format!("unknown codec {code}: {e}"))),
    }
}

/// Builder for [`Multisig`] objects. Mirrors `multi_sig::Builder`.
///
/// The `with_*` setters are fluent and return the same instance, so calls chain:
/// `new MultisigBuilder("eddsa-msig").withSignature(sig).withMessage(msg).tryBuild()`.
#[napi]
pub struct MultisigBuilder {
    inner: multi_sig::Builder,
}

#[napi]
impl MultisigBuilder {
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
    pub fn add_signature_share(&mut self, share: &Multisig) -> &Self {
        self.inner = std::mem::take(&mut self.inner).add_signature_share(&share.inner);
        self
    }

    /// Set the threshold disclosure mode for a share being built.
    ///
    /// `mode`: 0 = Full (t/n plaintext), 1 = Partial (n plaintext, t encrypted),
    /// 2 = FullConfidential (both encrypted). `metaKey` (32 bytes from
    /// [`MultisigBuilder::generateMetaKey`]) is required for modes 1 and 2.
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

    /// Construct from a BLS12-381 signature (`blsful::Signature`) with an explicit codec —
    /// `"bls12_381-g1-msig"` (48-byte sig) or `"bls12_381-g2-msig"` (96-byte sig).
    #[napi(factory)]
    pub fn new_from_bls_signature(codec: Either<String, u32>, signature: Buffer) -> Result<Self> {
        let c = resolve_codec(codec)?;
        let inner = match c {
            multi_sig::prelude::Codec::Bls12381G1Msig => {
                let sig = blsful::Signature::<blsful::Bls12381G1Impl>::try_from(signature.as_ref())
                    .map_err(|e| {
                        napi::Error::from_reason(format!("invalid BLS12-381 G1 signature: {e}"))
                    })?;
                multi_sig::Builder::new_from_bls_signature_with_codec(c, &sig)
                    .map_err(|e| napi::Error::from_reason(format!("bls signature: {e}")))?
            }
            multi_sig::prelude::Codec::Bls12381G2Msig => {
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
            multi_sig::prelude::Codec::Bls12381G1ShareMsig => {
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
            multi_sig::prelude::Codec::Bls12381G2ShareMsig => {
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

    /// Generate a fresh 32-byte threshold-metadata key, required by the Partial and
    /// FullConfidential disclosure modes.
    ///
    /// **Store it safely** — losing it makes `t` (and `n`) unrecoverable.
    #[napi]
    pub fn generate_meta_key() -> Buffer {
        Buffer::from(multi_sig::generate_meta_key().to_vec())
    }

    /// Build the [`Multisig`], folding in any accumulated shares.
    #[napi]
    pub fn try_build(&self) -> Result<Multisig> {
        self.inner
            .clone()
            .try_build()
            .map(|inner| Multisig { inner })
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
pub struct Crypt {
    pub(crate) repo: Arc<gix::ThreadSafeRepository>,
}

impl Crypt {
    /// A cheap thread-local view of the repository.
    fn gix(&self) -> gix::Repository {
        thread_local(&self.repo)
    }

    /// A [`KeyManager`] rooted at this repository's git directory.
    fn key_manager(&self) -> KeyManager {
        KeyManager::new(self.gix().git_dir())
    }

    /// Load the default key, ensuring the repository is initialized first.
    fn load_key(&self) -> CryptResult<CryptKey> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptError::NotInitialized);
        }
        km.load_key()
    }

    /// Path to the local git config file.
    fn config_path(&self) -> PathBuf {
        self.gix().git_dir().join("config")
    }

    /// Write `filter.git-crypt.*` into the local git config for git-CLI interop.
    fn configure_filters(&self) -> CryptResult<()> {
        let config_path = self.config_path();
        let mut config = if config_path.exists() {
            gix::config::File::from_path_no_includes(config_path.clone(), gix::config::Source::Local)
                .map_err(|e| CryptError::Git(e.to_string()))?
        } else {
            gix::config::File::new(gix::config::file::Metadata::from(gix::config::Source::Local))
        };

        config
            .set_raw_value(&"filter.git-crypt.clean", "git-crypt clean")
            .map_err(|e| CryptError::Git(e.to_string()))?;
        config
            .set_raw_value(&"filter.git-crypt.smudge", "git-crypt smudge")
            .map_err(|e| CryptError::Git(e.to_string()))?;
        config
            .set_raw_value(&"filter.git-crypt.diff", "git-crypt diff")
            .map_err(|e| CryptError::Git(e.to_string()))?;
        config
            .set_raw_value(&"filter.git-crypt.required", "true")
            .map_err(|e| CryptError::Git(e.to_string()))?;

        let mut file = std::fs::File::create(&config_path)?;
        config.write_to(&mut file)?;
        Ok(())
    }

    /// Remove the `filter.git-crypt.*` section from the local git config.
    fn remove_filters(&self) -> CryptResult<()> {
        let config_path = self.config_path();
        if !config_path.exists() {
            return Ok(());
        }
        let mut config = gix::config::File::from_path_no_includes(
            config_path.clone(),
            gix::config::Source::Local,
        )
        .map_err(|e| CryptError::Git(e.to_string()))?;
        let _ = config.remove_section("filter", Some("git-crypt".into()));
        let mut file = std::fs::File::create(&config_path)?;
        config.write_to(&mut file)?;
        Ok(())
    }
}

#[napi]
impl Crypt {
    /// Whether the repository has been initialized for crypt.
    #[napi]
    pub fn is_initialized(&self) -> bool {
        self.key_manager().is_initialized()
    }

    /// Initialize crypt: create the key directory, generate a key, and configure filters.
    /// Idempotent — a no-op if already initialized.
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

    /// Lock the repository by removing the `filter.crypt.*` config (encrypted files then
    /// show their encrypted content on checkout).
    #[napi]
    pub fn lock(&self) -> Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptError::NotInitialized.into());
        }
        self.remove_filters()?;
        Ok(())
    }

    /// Unlock the repository. If `key_file` is provided it is imported first, then the key
    /// is verified and the `filter.crypt.*` config is (re)written.
    #[napi]
    pub fn unlock(&self, key_file: Option<String>) -> Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptError::NotInitialized.into());
        }
        if let Some(kf) = key_file {
            km.import_key(Path::new(&kf))?;
        }
        let _key = km.load_key()?;
        self.configure_filters()?;
        Ok(())
    }

    /// Export the default key to `output_path`.
    #[napi]
    pub fn export_key(&self, output_path: String) -> Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptError::NotInitialized.into());
        }
        km.export_key(Path::new(&output_path))?;
        Ok(())
    }

    /// Import a key from `input_path` as the default key.
    #[napi]
    pub fn import_key(&self, input_path: String) -> Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptError::NotInitialized.into());
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

    /// Decrypt `data` produced by [`Crypt::encrypt`].
    #[napi]
    pub fn decrypt(&self, data: Buffer) -> Result<Buffer> {
        let key = self.load_key()?;
        let out = key.decrypt(data.as_ref())?;
        Ok(Buffer::from(out))
    }

    /// Whether `data` carries the crypt magic header.
    #[napi]
    pub fn is_encrypted(&self, data: Buffer) -> bool {
        CryptKey::is_encrypted(data.as_ref())
    }
}

/// Open the repository at `path` and return its git directory (used by the static,
/// repo-agnostic recipient-management helpers).
#[cfg(any(feature = "ssh", feature = "gpg"))]
fn open_repo_git_dir(path: &str) -> CryptResult<PathBuf> {
    let repo = gix::open(path).map_err(|e| CryptError::Git(format!("open repo: {e}")))?;
    Ok(repo.git_dir().to_path_buf())
}

#[cfg(feature = "ssh")]
#[napi]
impl Crypt {
    /// Add an SSH (ed25519) recipient (feature `ssh`). The repo's symmetric key is encrypted
    /// for the recipient's public key and written to `crypt/keys/ssh/<name>.ssh`.
    #[napi]
    pub fn add_ssh_user(&self, ssh_key_path: String, alias: Option<String>) -> Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptError::NotInitialized.into());
        }

        let key = km.load_key()?;
        let ssh_key = std::fs::read_to_string(&ssh_key_path)?;
        let encrypted_key = RingManager::encrypt_key_for_ssh_recipient(&key, &ssh_key)?;

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
            return Err(CryptError::NotInitialized.into());
        }

        let encrypted = std::fs::read(&encrypted_path)?;
        let identity = std::fs::read_to_string(&identity_path)?;
        let key = RingManager::decrypt_key_with_ssh_identity(
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
            // with the repo's *symmetric* git-crypt key). Using the symmetric key keeps the
            // committed `.pem` consistent with the `**.pem` git-crypt filter (so `smudge` works on
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
            commit_key_to_tree(&r, &target, &name, is_private, &patterns)?;
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
        String::from_utf8(bytes)
            .map_err(|_| napi::Error::from_reason("stored public key is not valid UTF-8".to_string()))
    }

    /// Ed25519-sign `data` with the stored seed at `refs/keys/<name>.pem` (default `sign`). The
    /// seed is decrypted with the repo's symmetric git-crypt key.
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
impl Crypt {
    /// Generate an XMSS^MT signing key (XMSSMT-SHA2_20/2_256) and store it in the vault:
    /// `refs/keys/<name>.xmss.vk` (public, plaintext) and `refs/keys/<name>.xmss.sk`
    /// (signing key, encrypted at rest under `SHA256(initial commit) || passphrase`).
    ///
    /// STATEFUL: the key yields a bounded number of signatures; sign with [`Crypt::sign_xmss`].
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
            return Err(napi::Error::from_reason("xmss signing key exhausted".to_string()));
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
        write_key_ref(&r, &format!("refs/keys/{name}.xmss.hwm"), &idx.to_be_bytes())?;

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
        let vk_bytes = read_ref_blob(&r, &format!("refs/keys/{name}.xmss.vk"))?.ok_or_else(
            || napi::Error::from_reason(format!("no xmss public key at refs/keys/{name}.xmss.vk")),
        )?;
        let vk = VerifyingKey::<XmssP>::try_from(vk_bytes.as_slice())
            .map_err(|e| napi::Error::from_reason(format!("decode xmss vk: {e}")))?;
        let sig = DetachedSignature::<XmssP>::try_from(signature.as_ref())
            .map_err(|e| napi::Error::from_reason(format!("decode xmss signature: {e}")))?;
        Ok(vk.verify_detached(&sig, data.as_ref()).is_ok())
    }
}

#[cfg(feature = "gpg")]
#[napi]
impl Crypt {
    /// Add a GPG recipient (feature `gpg`). Keyring lookup is not yet implemented, so this
    /// returns an actionable error rather than panicking. The repository is opened
    /// automatically from the current directory.
    #[napi]
    pub fn add_gpg_user(gpg_id: String) -> Result<()> {
        let git_dir = open_repo_git_dir(".")?;
        let km = KeyManager::new(&git_dir);
        if !km.is_initialized() {
            return Err(CryptError::NotInitialized.into());
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
pub fn clean_filter(key: &CryptKey) -> CryptResult<()> {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;
    if CryptKey::is_encrypted(&input) {
        io::stdout().write_all(&input)?;
        return Ok(());
    }
    let encrypted = key.encrypt(&input)?;
    io::stdout().write_all(&encrypted)?;
    Ok(())
}

/// Smudge filter: decrypt stdin to stdout (pass-through if not encrypted).
#[allow(dead_code)]
pub fn smudge_filter(key: &CryptKey) -> CryptResult<()> {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;
    if !CryptKey::is_encrypted(&input) {
        io::stdout().write_all(&input)?;
        return Ok(());
    }
    let decrypted = key.decrypt(&input)?;
    io::stdout().write_all(&decrypted)?;
    Ok(())
}

/// Diff filter: replace encrypted content with a placeholder notice.
#[allow(dead_code)]
pub fn diff_filter() -> CryptResult<()> {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;
    if CryptKey::is_encrypted(&input) {
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

/// SSH recipient support (feature = "ssh") — ed25519 keys only.
///
/// Encrypts the repo's symmetric key for a teammate's `ssh-ed25519` public key via X25519
/// ECDH (curve25519-dalek) and a `ring` AES-256-GCM envelope; decryption uses the teammate's
/// OpenSSH ed25519 *private* key (unencrypted, or passphrase-protected via the `ssh-key`
/// crate). There is no `age`/`rage` dependency.
#[cfg(feature = "ssh")]
pub struct RingManager;

#[cfg(feature = "ssh")]
impl RingManager {
    /// Magic header for the encrypted-recipient-key envelope.
    const MAGIC: &[u8; 8] = b"RINGSSH1";

    /// Encrypt the repo's symmetric key so only the holder of the matching ed25519 SSH
    /// private key can recover it. `recipient` is the contents of an `ssh-ed25519` public
    /// (or private) key file.
    pub fn encrypt_key_for_ssh_recipient(key: &CryptKey, recipient: &str) -> CryptResult<Vec<u8>> {
        let recipient_pk = ssh_ed25519_public_bytes(recipient)?;
        let x25519_pk = ed25519_to_x25519_public(&recipient_pk)?;

        // Ephemeral X25519 keypair (random).
        let mut ephemeral = [0u8; 32];
        SystemRandom::new()
            .fill(&mut ephemeral)
            .map_err(|_| CryptError::Ssh("RNG failure".into()))?;
        let esk = X25519Secret::from(ephemeral);
        let epk = X25519PublicKey::from(&esk);

        let shared = esk.diffie_hellman(&x25519_pk);
        let aead_key = derive_aead_key(shared.as_bytes())?;

        let mut nonce = [0u8; NONCE_SIZE];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| CryptError::Ssh("RNG failure".into()))?;

        let ciphertext = aead_seal(&aead_key, &nonce, key.as_bytes())?;

        let mut out = Vec::with_capacity(8 + 32 + NONCE_SIZE + ciphertext.len());
        out.extend_from_slice(Self::MAGIC);
        out.extend_from_slice(epk.as_bytes());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Decrypt a `RingManager` envelope using an OpenSSH ed25519 private key. When the
    /// private key is passphrase-protected, `passphrase` must be supplied.
    pub fn decrypt_key_with_ssh_identity(
        encrypted: &[u8],
        identity_content: &str,
        passphrase: Option<&str>,
    ) -> CryptResult<CryptKey> {
        if encrypted.len() < 8 + 32 + NONCE_SIZE {
            return Err(CryptError::Ssh("envelope too short".into()));
        }
        if &encrypted[..8] != Self::MAGIC {
            return Err(CryptError::Ssh("invalid envelope magic".into()));
        }
        let (_, rest) = encrypted.split_at(8);
        let (epk_bytes, rest) = rest.split_at(32);
        let (nonce_bytes, ct) = rest.split_at(NONCE_SIZE);

        let mut epk_arr = [0u8; 32];
        epk_arr.copy_from_slice(epk_bytes);
        let epk = X25519PublicKey::from(epk_arr);

        let seed = ssh_ed25519_seed(identity_content, passphrase)?;
        let scalar = ed25519_seed_to_x25519_scalar(&seed);
        let sk = X25519Secret::from(scalar);
        let shared = sk.diffie_hellman(&epk);
        let aead_key = derive_aead_key(shared.as_bytes())?;

        let mut nonce_arr = [0u8; NONCE_SIZE];
        nonce_arr.copy_from_slice(nonce_bytes);

        let key_bytes = aead_open(&aead_key, &nonce_arr, ct)?;
        CryptKey::from_bytes(&key_bytes)
    }
}

// ---------------------------------------------------------------------------
// SSH key parsing / conversion helpers (feature = "ssh")
// ---------------------------------------------------------------------------

/// Parse an `ssh-ed25519` public key (or a private key, extracting its public) into the
/// 32-byte Edwards-point encoding.
#[cfg(feature = "ssh")]
fn ssh_ed25519_public_bytes(recipient: &str) -> CryptResult<[u8; 32]> {
    if let Ok(pubkey) = ssh_key::PublicKey::from_openssh(recipient.trim()) {
        if let Ok(bytes) = ed25519_pub_from_ssh_public(&pubkey) {
            return Ok(bytes);
        }
    }
    let privkey = ssh_key::PrivateKey::from_openssh(recipient.trim())
        .map_err(|e| CryptError::Ssh(format!("invalid SSH key: {e}")))?;
    ed25519_pub_from_ssh_private(&privkey)
}

#[cfg(feature = "ssh")]
fn ed25519_pub_from_ssh_public(pubkey: &ssh_key::PublicKey) -> CryptResult<[u8; 32]> {
    use ssh_key::public::KeyData;
    match pubkey.key_data() {
        KeyData::Ed25519(p) => {
            let b: &[u8] = p.as_ref();
            if b.len() != 32 {
                return Err(CryptError::Ssh("bad ed25519 public key".into()));
            }
            let mut a = [0u8; 32];
            a.copy_from_slice(b);
            Ok(a)
        }
        _ => Err(CryptError::Ssh("only ed25519 SSH keys are supported".into())),
    }
}

#[cfg(feature = "ssh")]
fn ed25519_pub_from_ssh_private(privkey: &ssh_key::PrivateKey) -> CryptResult<[u8; 32]> {
    use ssh_key::private::KeypairData;
    match privkey.key_data() {
        KeypairData::Ed25519(kp) => {
            let b: &[u8] = kp.public.as_ref();
            if b.len() != 32 {
                return Err(CryptError::Ssh("bad ed25519 public key".into()));
            }
            let mut a = [0u8; 32];
            a.copy_from_slice(b);
            Ok(a)
        }
        _ => Err(CryptError::Ssh("only ed25519 SSH keys are supported".into())),
    }
}

#[cfg(feature = "ssh")]
fn ssh_ed25519_seed(identity_content: &str, passphrase: Option<&str>) -> CryptResult<[u8; 32]> {
    use ssh_key::private::KeypairData;
    let privkey = if let Some(pw) = passphrase {
        ssh_key::PrivateKey::from_openssh(identity_content.trim())
            .and_then(|k| k.decrypt(pw))
            .map_err(|e| CryptError::Ssh(format!("failed to decrypt SSH identity: {e}")))
    } else {
        ssh_key::PrivateKey::from_openssh(identity_content.trim()).map_err(|e| {
            CryptError::Ssh(format!("invalid/unencrypted SSH identity (passphrase needed?): {e}"))
        })
    }?;
    match privkey.key_data() {
        KeypairData::Ed25519(kp) => Ok(kp.private.to_bytes()),
        _ => Err(CryptError::Ssh("only ed25519 SSH keys are supported".into())),
    }
}

/// Convert an ed25519 public point into an X25519 public key (Edwards -> Montgomery).
#[cfg(feature = "ssh")]
fn ed25519_to_x25519_public(edwards: &[u8; 32]) -> CryptResult<X25519PublicKey> {
    use curve25519_dalek::edwards::CompressedEdwardsY;
    use curve25519_dalek::montgomery::MontgomeryPoint;
    let compressed = CompressedEdwardsY::from_slice(edwards)
        .map_err(|_| CryptError::Ssh("invalid ed25519 public point".into()))?;
    let point = compressed
        .decompress()
        .ok_or_else(|| CryptError::Ssh("invalid ed25519 public point".into()))?;
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
fn derive_aead_key(shared: &[u8]) -> CryptResult<[u8; 32]> {
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, b"gitcrypt-ssh-v1");
    let prk = salt.extract(shared);
    let okm = prk
        .expand(&[b"ssh-recipient-key"], hkdf::HKDF_SHA256)
        .map_err(|_| CryptError::Ssh("hkdf expand failed".into()))?;
    let mut key = [0u8; 32];
    okm.fill(&mut key)
        .map_err(|_| CryptError::Ssh("hkdf fill failed".into()))?;
    Ok(key)
}

#[cfg(feature = "ssh")]
fn aead_seal(key: &[u8; 32], nonce: &[u8; NONCE_SIZE], plaintext: &[u8]) -> CryptResult<Vec<u8>> {
    let unbound = UnboundKey::new(&AES_256_GCM, key)
        .map_err(|_| CryptError::Ssh("invalid aead key".into()))?;
    let key = LessSafeKey::new(unbound);
    let nonce = Nonce::try_assume_unique_for_key(nonce)
        .map_err(|_| CryptError::Ssh("invalid nonce".into()))?;
    let mut in_out = plaintext.to_vec();
    key.seal_in_place_append_tag(nonce, Aad::empty(), &mut in_out)
        .map_err(|_| CryptError::Ssh("encryption failed".into()))?;
    Ok(in_out)
}

#[cfg(feature = "ssh")]
fn aead_open(key: &[u8; 32], nonce: &[u8; NONCE_SIZE], ciphertext: &[u8]) -> CryptResult<Vec<u8>> {
    let unbound = UnboundKey::new(&AES_256_GCM, key)
        .map_err(|_| CryptError::Ssh("invalid aead key".into()))?;
    let key = LessSafeKey::new(unbound);
    let nonce = Nonce::try_assume_unique_for_key(nonce)
        .map_err(|_| CryptError::Ssh("invalid nonce".into()))?;
    let mut in_out = ciphertext.to_vec();
    let plain = key
        .open_in_place(nonce, Aad::empty(), &mut in_out)
        .map_err(|_| CryptError::Ssh("decryption failed (wrong key or tampered)".into()))?;
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
fn initial_commit_id(r: &gix::Repository) -> CryptResult<gix::ObjectId> {
    // 1. Explicit pin, if present.
    if let Ok(mut reference) = r.find_reference("refs/heads/meta/repository") {
        if let Ok(id) = reference.peel_to_id() {
            return Ok(id.detach());
        }
    }
    // 2. Derive: walk first-parent from HEAD down to the root commit.
    let head = r
        .head_commit()
        .map_err(|e| CryptError::Git(format!("head_commit (repo has no commits?): {e}")))?;
    let mut current = head.id().detach();
    loop {
        let commit = r
            .find_commit(current)
            .map_err(|e| CryptError::Git(format!("find_commit: {e}")))?;
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
fn vault_seal(key: &[u8; 32], plaintext: &[u8]) -> CryptResult<Vec<u8>> {
    let mut nonce = [0u8; NONCE_SIZE];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| CryptError::Ssh("RNG failure".into()))?;
    let ct = aead_seal(key, &nonce, plaintext)?;
    let mut out = Vec::with_capacity(VAULT_MAGIC.len() + NONCE_SIZE + ct.len());
    out.extend_from_slice(VAULT_MAGIC);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Decrypt a stored private-key envelope produced by [`vault_seal`].
#[cfg(feature = "ssh")]
fn vault_open(key: &[u8; 32], blob: &[u8]) -> CryptResult<Vec<u8>> {
    let min = VAULT_MAGIC.len() + NONCE_SIZE;
    if blob.len() < min || &blob[..VAULT_MAGIC.len()] != VAULT_MAGIC {
        return Err(CryptError::Ssh("invalid key envelope".into()));
    }
    let (_, rest) = blob.split_at(VAULT_MAGIC.len());
    let (nonce_bytes, ct) = rest.split_at(NONCE_SIZE);
    let mut nonce = [0u8; NONCE_SIZE];
    nonce.copy_from_slice(nonce_bytes);
    aead_open(key, &nonce, ct)
}

/// Write `bytes` as a blob and point the ref `full_name` at it (creating or updating).
#[cfg(feature = "ssh")]
fn write_key_ref(r: &gix::Repository, full_name: &str, bytes: &[u8]) -> CryptResult<()> {
    let oid = r
        .write_blob(bytes.to_vec())
        .map_err(|e| CryptError::Git(format!("write_blob: {e}")))?;
    r.reference(
        full_name,
        oid,
        gix::refs::transaction::PreviousValue::Any,
        "crypt: store key",
    )
    .map_err(|e| CryptError::Git(format!("create ref {full_name}: {e}")))?;
    Ok(())
}

/// Read the blob referenced by `full_name`, or `None` if the ref does not exist.
#[cfg(feature = "ssh")]
fn read_ref_blob(r: &gix::Repository, full_name: &str) -> CryptResult<Option<Vec<u8>>> {
    let reference = match r.try_find_reference(full_name) {
        Ok(Some(rf)) => rf,
        Ok(None) => return Ok(None),
        Err(e) => return Err(CryptError::Git(format!("find_reference: {e}"))),
    };
    let id = match reference.try_id() {
        Some(id) => id.detach(),
        None => return Err(CryptError::Git(format!("{full_name} is a symbolic ref"))),
    };
    let blob = r
        .find_blob(id)
        .map_err(|e| CryptError::Git(format!("find_blob: {e}")))?;
    Ok(Some(blob.data.to_vec()))
}

/// List the distinct key names stored under `refs/keys/*` (stripping `.pub`/`.pem`).
#[cfg(feature = "ssh")]
fn list_key_names(r: &gix::Repository) -> CryptResult<Vec<String>> {
    use std::collections::BTreeSet;
    let platform = r
        .references()
        .map_err(|e| CryptError::Git(format!("references: {e}")))?;
    let iter = platform
        .prefixed(b"refs/keys/")
        .map_err(|e| CryptError::Git(format!("prefixed: {e}")))?;
    let mut names = BTreeSet::new();
    for item in iter {
        let reference = item.map_err(|e| CryptError::Git(format!("iterate refs: {e}")))?;
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
fn parse_ed25519_private(pem: &str, passphrase: &str) -> CryptResult<([u8; 32], [u8; 32])> {
    use ssh_key::private::KeypairData;

    // 1. OpenSSH private key.
    if let Ok(privkey) = ssh_key::PrivateKey::from_openssh(pem.trim()) {
        let privkey = if privkey.is_encrypted() {
            if passphrase.is_empty() {
                return Err(CryptError::Ssh("private key is passphrase-protected".into()));
            }
            privkey
                .decrypt(passphrase)
                .map_err(|e| CryptError::Ssh(format!("decrypt SSH key: {e}")))?
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
            _ => Err(CryptError::Ssh("only ed25519 keys are supported".into())),
        };
    }

    // 2. PKCS#8 private key (unencrypted). Encrypted PKCS#8 must be decrypted by the caller first.
    let kp = ed25519::pkcs8::KeypairBytes::from_pkcs8_pem(pem.trim()).map_err(|e| {
        CryptError::Ssh(format!(
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
fn parse_ed25519_public(pem: &str) -> CryptResult<[u8; 32]> {
    // 1. OpenSSH public key.
    if let Ok(pk) = ssh_key::PublicKey::from_openssh(pem.trim()) {
        return ed25519_pub_from_ssh_public(&pk);
    }
    // 2. PKCS#8 SubjectPublicKeyInfo PEM.
    let pk = ed25519::pkcs8::PublicKeyBytes::from_public_key_pem(pem.trim())
        .map_err(|e| CryptError::Ssh(format!("unrecognized ed25519 public key: {e}")))?;
    Ok(pk.to_bytes())
}

/// Build the OpenSSH text form (`ssh-ed25519 AAAA...`) for a raw 32-byte ed25519 public key.
#[cfg(feature = "ssh")]
fn ed25519_public_openssh_text(public: &[u8; 32]) -> CryptResult<String> {
    let mut wire = Vec::with_capacity(4 + 11 + 4 + 32);
    wire.extend_from_slice(&(11u32).to_be_bytes());
    wire.extend_from_slice(b"ssh-ed25519");
    wire.extend_from_slice(&(32u32).to_be_bytes());
    wire.extend_from_slice(public);
    let pk = ssh_key::PublicKey::from_bytes(&wire)
        .map_err(|e| CryptError::Ssh(format!("build public key: {e}")))?;
    pk.to_openssh()
        .map_err(|e| CryptError::Ssh(format!("serialize public key: {e}")))
}

/// Derive the ed25519 public key from a 32-byte seed.
#[cfg(feature = "ssh")]
fn derive_ed25519_public(seed: &[u8; 32]) -> CryptResult<[u8; 32]> {
    use ring::signature::KeyPair as _;
    let kp = signature::Ed25519KeyPair::from_seed_unchecked(seed)
        .map_err(|_| CryptError::Ssh("invalid ed25519 seed".into()))?;
    let mut public = [0u8; 32];
    public.copy_from_slice(kp.public_key().as_ref());
    Ok(public)
}

/// Ed25519-sign `data` with a raw 32-byte seed, returning the 64-byte signature.
#[cfg(feature = "ssh")]
fn sign_with_seed(seed: &[u8; 32], data: &[u8]) -> CryptResult<Vec<u8>> {
    let kp = signature::Ed25519KeyPair::from_seed_unchecked(seed)
        .map_err(|_| CryptError::Ssh("invalid ed25519 seed".into()))?;
    Ok(kp.sign(data).as_ref().to_vec())
}

/// Find the stored `refs/keys/*.pub` whose public key matches `identity_pub`, returning its
/// name and OpenSSH text.
#[cfg(feature = "ssh")]
fn find_stored_public(
    r: &gix::Repository,
    identity_pub: &[u8; 32],
) -> CryptResult<Option<(String, String)>> {
    let platform = r
        .references()
        .map_err(|e| CryptError::Git(format!("references: {e}")))?;
    let iter = platform
        .prefixed(b"refs/keys/")
        .map_err(|e| CryptError::Git(format!("prefixed: {e}")))?;
    for item in iter {
        let reference = item.map_err(|e| CryptError::Git(format!("iterate refs: {e}")))?;
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
fn xmss_read_index(sk: &[u8]) -> CryptResult<u64> {
    if sk.len() < 4 + XMSS_INDEX_BYTES {
        return Err(CryptError::Xmss("signing key too short".into()));
    }
    let mut idx = 0u64;
    for &b in &sk[4..4 + XMSS_INDEX_BYTES] {
        idx = (idx << 8) | u64::from(b);
    }
    Ok(idx)
}

/// Options for storing a key via [`Crypt::add_key`].
#[cfg(feature = "ssh")]
#[napi(object)]
pub struct KeyStoreOptions {
    /// When set, also commit the key material as a new commit. A full ref
    /// (`refs/heads/meta/repository`) or short name (`meta/repository`) extends that branch's tip;
    /// the sentinel `"/"` creates an *orphan* (parentless) commit on `refs/heads/<name>`.
    /// Default (`undefined`): only write refs under `refs/keys/*`, no commit.
    pub commit_to_tree: Option<String>,
    /// `.gitattributes` patterns to write (each gets `filter=git-crypt diff=git-crypt` appended).
    /// Defaults to a small sensible set.
    pub gitattributes: Option<Vec<String>>,
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
) -> CryptResult<()> {
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
            .map_err(|e| CryptError::Git(format!("find {full_ref}: {e}")))?;
        let tip = reference
            .peel_to_id()
            .map_err(|e| CryptError::Git(format!("peel {full_ref}: {e}")))?
            .detach();
        let tree = r
            .find_commit(tip)
            .map_err(|e| CryptError::Git(format!("find_commit: {e}")))?
            .tree_id()
            .map_err(|e| CryptError::Git(format!("tree_id: {e}")))?
            .detach();
        (Some(tip), tree)
    };

    let mut editor = r
        .edit_tree(base_tree)
        .map_err(|e| CryptError::Git(format!("edit_tree: {e}")))?;

    // .gitattributes declaring the encrypted file patterns.
    let mut attrs = String::new();
    for p in patterns {
        attrs.push_str(p);
        attrs.push_str(" filter=git-crypt diff=git-crypt\n");
    }
    let attrs_blob = r
        .write_blob(attrs.into_bytes())
        .map_err(|e| CryptError::Git(format!("write_blob: {e}")))?;
    editor
        .upsert(".gitattributes", gix::objs::tree::EntryKind::Blob, attrs_blob)
        .map_err(|e| CryptError::Git(format!("upsert .gitattributes: {e}")))?;

    // .<name>.pub (public key, plaintext).
    if let Some(pub_bytes) = read_ref_blob(r, &format!("refs/keys/{name}.pub"))? {
        let blob = r
            .write_blob(pub_bytes)
            .map_err(|e| CryptError::Git(format!("write_blob: {e}")))?;
        editor
            .upsert(format!(".{name}.pub"), gix::objs::tree::EntryKind::Blob, blob)
            .map_err(|e| CryptError::Git(format!("upsert .{name}.pub: {e}")))?;
    }

    // .<name>.pem (encrypted-at-rest private key).
    if include_private {
        if let Some(pem_bytes) = read_ref_blob(r, &format!("refs/keys/{name}.pem"))? {
            let blob = r
                .write_blob(pem_bytes)
                .map_err(|e| CryptError::Git(format!("write_blob: {e}")))?;
            editor
                .upsert(format!(".{name}.pem"), gix::objs::tree::EntryKind::Blob, blob)
                .map_err(|e| CryptError::Git(format!("upsert .{name}.pem: {e}")))?;
        }
    }

    let new_tree = editor
        .write()
        .map_err(|e| CryptError::Git(format!("write tree: {e}")))?
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
        .map_err(|e| CryptError::Git(format!("write_commit: {e}")))?;
    r.reference(
        full_ref.as_str(),
        commit_id,
        gix::refs::transaction::PreviousValue::Any,
        "crypt: commit key",
    )
    .map_err(|e| CryptError::Git(format!("update ref {full_ref}: {e}")))?;
    Ok(())
}

/// An ed25519 SSH identity, exposed to JS for asymmetric `sign`/`verify`.
///
/// NOTE: this is separate from [`CryptoKey`], which is *symmetric* (only
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
    pub fn encrypt_key_for_recipient(_key: &CryptKey, _recipient: &str) -> CryptResult<Vec<u8>> {
        Err(CryptError::Gpg(
            "GPG recipient encryption is not yet implemented (keyring lookup pending)".into(),
        ))
    }
}

#[napi(object)]
pub struct KeyAlgorithm {
    /// The algorithm name, e.g. `"AES-GCM"`, `"RSASSA-PKCS1-v1_5"`, `"ECDSA"`.
    pub name: String,
}

/// RSA key algorithm with an associated hash.
#[napi(object)]
pub struct RsaHashedKeyAlgorithm {
    /// The algorithm name, e.g. `"RSASSA-PKCS1-v1_5"`.
    pub name: String,
    /// RSA modulus length in bits.
    pub modulus_length: u32,
    /// RSA public exponent (BigInt serialized as bytes, e.g. 65537 -> `010001`).
    pub public_exponent: Buffer,
    /// The associated hashing algorithm.
    pub hash: KeyAlgorithm,
}

/// Elliptic-curve key algorithm.
#[napi(object)]
pub struct EcKeyAlgorithm {
    /// The algorithm name, e.g. `"ECDSA"`.
    pub name: String,
    /// The named curve, e.g. `"P-256"`.
    pub named_curve: String,
}

/// AES key algorithm.
#[napi(object)]
pub struct AesKeyAlgorithm {
    /// The algorithm name, e.g. `"AES-GCM"`.
    pub name: String,
    /// Key length in bits (128, 192, or 256).
    pub length: u32,
}

/// HMAC key algorithm.
#[napi(object)]
pub struct HmacKeyAlgorithm {
    /// The algorithm name, e.g. `"HMAC"`.
    pub name: String,
    /// MAC length in bits (optional per spec).
    pub length: Option<u32>,
    /// The associated hashing algorithm (optional per spec).
    pub hash: Option<KeyAlgorithm>,
}

/// KMAC key algorithm.
#[napi(object)]
pub struct KmacKeyAlgorithm {
    /// The algorithm name, e.g. `"KMAC"`.
    pub name: String,
    /// MAC length in bits (optional per spec).
    pub length: Option<u32>,
}

/// Key formats supported for export and import operations.
#[napi]
pub enum CryptoKeyFormat {
    Raw,
    Pkcs8,
    Spki,
    Jwk,
    RawSecret,
    RawPublic,
    RawSeed,
}

/// A Web Crypto `CryptoKey`.
///
/// Read-only from JS: Node treats these properties as immutable. `#[napi(object)]`
/// generates a JS constructor accepting the literal `{ algorithm, extractable,
/// type, usages }`; the fields are surfaced as getters and should not be mutated.
#[napi(object)]
pub struct CryptoKey {
    /// The algorithm (and algorithm-specific parameters) the key is for.
    pub algorithm:
        Either6<AesKeyAlgorithm, HmacKeyAlgorithm, EcKeyAlgorithm, RsaHashedKeyAlgorithm, KmacKeyAlgorithm, KeyAlgorithm>,
    /// Whether the key may be exported via `subtle.exportKey` / `wrapKey`.
    pub extractable: bool,
    /// `"secret"` (symmetric), `"private"`, or `"public"` (asymmetric).
    #[napi(js_name = "type")]
    pub key_type: String,
    /// The operations this key may be used for.
    /// One or more of: encrypt, decrypt, sign, verify, deriveKey, deriveBits,
    /// encapsulateBits, decapsulateBits, encapsulateKey, decapsulateKey,
    /// wrapKey, unwrapKey.
    pub usages: Vec<String>,
}

/// A Web Crypto `CryptoKeyPair`: an asymmetric key pair.
#[napi(object)]
pub struct CryptoKeyPair {
    /// The public half (`type === "public"`).
    pub public_key: CryptoKey,
    /// The private half (`type === "private"`).
    pub private_key: CryptoKey,
}

/// Signature helper constructor taking key format.
#[napi]
pub struct Signature {
    pub key_format: CryptoKeyFormat,
}

#[napi]
impl Signature {
    #[napi(constructor)]
    pub fn new(key_format: CryptoKeyFormat) -> Self {
        Self { key_format }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn deterministic_key(byte: u8) -> CryptKey {
        CryptKey::from_bytes(&vec![byte; KEY_SIZE]).unwrap()
    }

    // ---- CryptKey ----

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = CryptKey::generate();
        let plaintext = b"Hello, World!";
        let ciphertext = key.encrypt(plaintext).unwrap();
        assert_ne!(plaintext.as_slice(), &ciphertext[..]);
        let decrypted = key.decrypt(&ciphertext).unwrap();
        assert_eq!(plaintext.as_slice(), &decrypted[..]);
    }

    #[test]
    fn encrypt_is_randomized_and_detected() {
        let key = CryptKey::generate();
        let c1 = key.encrypt(b"same").unwrap();
        let c2 = key.encrypt(b"same").unwrap();
        assert_ne!(c1, c2, "fresh nonce per call");
        assert!(CryptKey::is_encrypted(&c1));
        assert!(!CryptKey::is_encrypted(b"plain"));
    }

    #[test]
    fn wrong_key_fails_and_tamper_detected() {
        let key1 = CryptKey::generate();
        let key2 = CryptKey::generate();
        let ct = key1.encrypt(b"secret").unwrap();
        assert!(key2.decrypt(&ct).is_err());

        let mut tampered = ct.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xFF;
        assert!(key1.decrypt(&tampered).is_err());
    }

    #[test]
    fn empty_and_invalid_format() {
        let key = CryptKey::generate();
        let ct = key.encrypt(b"").unwrap();
        assert_eq!(key.decrypt(&ct).unwrap(), b"".to_vec());
        assert!(key.decrypt(&[1, 2, 3]).is_err());
        assert!(CryptKey::from_bytes(&[0u8; KEY_SIZE - 1]).is_err());
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

        let repo_key = CryptKey::generate();
        let enc = RingManager::encrypt_key_for_ssh_recipient(&repo_key, &pub_string).unwrap();
        let dec = RingManager::decrypt_key_with_ssh_identity(&enc, &pem, None).unwrap();
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
        const A: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b0 = chunk[0] as usize;
            let b1 = if chunk.len() > 1 { chunk[1] as usize } else { 0 };
            let b2 = if chunk.len() > 2 { chunk[2] as usize } else { 0 };
            let n = (b0 << 16) | (b1 << 8) | b2;
            out.push(A[(n >> 18) & 63] as char);
            out.push(A[(n >> 12) & 63] as char);
            out.push(if chunk.len() > 1 { A[(n >> 6) & 63] as char } else { '=' });
            out.push(if chunk.len() > 2 { A[n & 63] as char } else { '=' });
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
        format!(
            "-----BEGIN OPENSSH PRIVATE KEY-----\n{b64}\n-----END OPENSSH PRIVATE KEY-----"
        )
    }
}
