use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chacha20poly1305::ChaCha20Poly1305;
use chacha20poly1305::aead::{Aead, Key, KeyInit, Nonce};
#[cfg(all(feature = "multi-sig", feature = "ssh"))]
use multi_sig::CodecInfo as _;
#[cfg(all(feature = "multi-sig", feature = "ssh"))]
use multi_sig::DataView as _;
use napi::bindgen_prelude::Buffer;
#[cfg(any(feature = "multi-sig", feature = "btls"))]
use napi::bindgen_prelude::Either;
use napi_derive::napi;

use thiserror::Error;

#[cfg(feature = "ssh")]
use multi_codec::Codec::Ed25519Pub;
#[cfg(feature = "xmss")]
use multi_codec::Codec::XmssSha220256Msig;

use crate::error::thread_local;
#[cfg(all(feature = "multi-sig", feature = "ssh"))]
use multi_sig::CodecInfo;

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

    #[error("gpg error: {0}")]
    Gpg(String),

    #[cfg(feature = "xmss")]
    #[error("xmss error: {0}")]
    Xmss(String),
}

/// Alias for internal, typed results.
pub type CryptoResult<T> = std::result::Result<T, CryptoError>;

impl From<CryptoError> for napi::Error {
    fn from(e: CryptoError) -> Self {
        napi::Error::from_reason(e.to_string())
    }
}

/// Reverse conversion so internal `CryptoResult`-returning helpers can `?`-propagate
/// errors produced by NAPI-typed helpers (e.g. `SymmetricCryptoKey::from_bytes`).
impl From<napi::Error> for CryptoError {
    fn from(e: napi::Error) -> Self {
        CryptoError::Crypto(e.to_string())
    }
}

/// Key size in bytes (256 bits).
pub const KEY_SIZE: usize = 32;
/// Nonce size in bytes (96 bits, ChaCha20-Poly1305 standard).
pub const NONCE_SIZE: usize = 12;
/// Magic header identifying crypt-encrypted payloads.
const MAGIC_HEADER: &[u8] = b"GITCRYPT";

/// Repository encryption mode.
#[napi]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CryptoMode {
    /// File-level clean/smudge filter encryption (git-crypt partial mode).
    Filter = 0,
    /// Entire repository packfile & manifest encryption (git-remote-gcrypt mode).
    EntireRepo = 1,
    /// Layered/Hybrid mode: partial file filtering (git-crypt) + entire repository encryption (gcrypt).
    Hybrid = 2,
}

#[napi(object)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ManifestRef {
    pub object_id: String,
    pub name: String,
}

#[napi(object)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ManifestPack {
    pub hash_type: String,
    pub pack_id: String,
    pub key: String,
}

#[napi(object)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ManifestKeep {
    pub hash_type: String,
    pub pack_id: String,
    pub count: u32,
}

/// Representation of a git-remote-gcrypt compatible repository manifest.
#[napi(object)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoManifest {
    pub refs: Vec<ManifestRef>,
    pub packs: Vec<ManifestPack>,
    pub keep: Vec<ManifestKeep>,
    pub repo_id: String,
    pub extensions: Vec<String>,
}

/// Serialize a RepoManifest object into standard gcrypt manifest format.
#[napi]
pub fn serialize_manifest(manifest: RepoManifest) -> String {
    let mut lines = Vec::new();

    // 1. Refs list: <hex40> <ref_name>
    for r in &manifest.refs {
        lines.push(format!("{} {}", r.object_id, r.name));
    }

    // 2. Pack list: pack :<hashtype>:<pack_id> <key>
    for p in &manifest.packs {
        let ht = if p.hash_type.is_empty() {
            "SHA256"
        } else {
            &p.hash_type
        };
        lines.push(format!("pack :{ht}:{} {}", p.pack_id, p.key));
    }

    // 3. Keep list: keep :<hashtype>:<pack_id> <count>
    for k in &manifest.keep {
        let ht = if k.hash_type.is_empty() {
            "SHA256"
        } else {
            &k.hash_type
        };
        lines.push(format!("keep :{ht}:{} {}", k.pack_id, k.count));
    }

    // 4. Repo ID: repo <repo_id>
    if !manifest.repo_id.is_empty() {
        if manifest.repo_id.starts_with("repo ") {
            lines.push(manifest.repo_id.clone());
        } else {
            lines.push(format!("repo {}", manifest.repo_id));
        }
    }

    // 5. Extensions: extn <ext>
    for e in &manifest.extensions {
        lines.push(format!("extn {e}"));
    }

    lines.join("\n")
}

/// Parse a manifest string in standard gcrypt format.
#[napi]
pub fn parse_manifest(text: String) -> napi::Result<RepoManifest> {
    let mut manifest = RepoManifest::default();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }

        if parts[0] == "pack" && parts.len() >= 3 {
            let pack_spec = parts[1];
            let key = parts[2..].join(" ");
            let sub: Vec<&str> = pack_spec.split(':').collect();
            if sub.len() >= 3 {
                manifest.packs.push(ManifestPack {
                    hash_type: sub[1].to_string(),
                    pack_id: sub[2].to_string(),
                    key,
                });
            }
        } else if parts[0] == "keep" && parts.len() >= 3 {
            let keep_spec = parts[1];
            let count: u32 = parts[2].parse().unwrap_or(1);
            let sub: Vec<&str> = keep_spec.split(':').collect();
            if sub.len() >= 3 {
                manifest.keep.push(ManifestKeep {
                    hash_type: sub[1].to_string(),
                    pack_id: sub[2].to_string(),
                    count,
                });
            }
        } else if parts[0] == "repo" && parts.len() >= 2 {
            manifest.repo_id = parts[1..].join(" ");
        } else if parts[0] == "extn" && parts.len() >= 2 {
            manifest.extensions.push(parts[1..].join(" "));
        } else if parts.len() == 2
            && (parts[0].len() == 40 || parts[0].len() == 64)
            && parts[0].chars().all(|c| c.is_ascii_hexdigit())
        {
            manifest.refs.push(ManifestRef {
                object_id: parts[0].to_string(),
                name: parts[1].to_string(),
            });
        }
    }

    Ok(manifest)
}

/// Result of encrypting a packfile for full repo mode.
#[napi(object)]
pub struct EncryptedPackResult {
    pub pack_id: String,
    pub hash_type: String,
    pub pack_key: String,
    pub encrypted_bytes: Buffer,
}

/// A symmetric ChaCha20-Poly1305 key. Exposed to JS as the `SymmetricCryptoKey` class.
#[napi(js_name = "SymmetricCryptoKey")]
#[derive(Clone)]
pub struct SymmetricCryptoKey {
    key: [u8; KEY_SIZE],
}

#[napi]
impl SymmetricCryptoKey {
    #[napi(constructor)]
    pub fn new(bytes: Option<&[u8]>) -> napi::Result<Self> {
        match bytes {
            Some(bytes) => Ok(Self::from_bytes(bytes)?),
            None => Ok(Self::generate_random()),
        }
    }

    /// Generate a new random key from the OS CSPRNG.
    #[napi(factory)]
    pub fn generate_random() -> Self {
        use rand::RngCore as _;
        let mut key = [0u8; KEY_SIZE];
        rand::thread_rng().fill_bytes(&mut key);
        Self { key }
    }

    /// Build a key from raw bytes, rejecting any length other than [`KEY_SIZE`].
    #[napi]
    pub fn from_bytes(bytes: &[u8]) -> napi::Result<SymmetricCryptoKey> {
        if bytes.len() != KEY_SIZE {
            return Err(CryptoError::InvalidKeyFormat.into());
        }
        let mut key = [0u8; KEY_SIZE];
        key.copy_from_slice(bytes);
        Ok(Self { key })
    }

    /// Borrow the raw key bytes in Rust.
    pub fn as_bytes(&self) -> &[u8] {
        &self.key
    }

    /// Borrow the raw key bytes as a Buffer for JS.
    #[napi(js_name = "asBytes")]
    pub fn napi_as_bytes(&self) -> Buffer {
        Buffer::from(self.key.to_vec())
    }

    /// Encrypt `plaintext` into `MAGIC_HEADER || nonce || ciphertext+tag` (ChaCha20-Poly1305).
    pub fn encrypt_raw(&self, plaintext: &[u8]) -> CryptoResult<Vec<u8>> {
        use rand::RngCore as _;
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);

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

    /// Encrypt `plaintext` Buffer into `MAGIC_HEADER || nonce || ciphertext+tag` (ChaCha20-Poly1305).
    #[napi]
    pub fn encrypt(&self, plaintext: Buffer) -> napi::Result<Buffer> {
        let out = self.encrypt_raw(plaintext.as_ref())?;
        Ok(Buffer::from(out))
    }

    /// Decrypt a payload produced by [`SymmetricCryptoKey::encrypt_raw`].
    pub fn decrypt_raw(&self, ciphertext: &[u8]) -> CryptoResult<Vec<u8>> {
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

    /// Decrypt a payload produced by [`SymmetricCryptoKey::encrypt`].
    #[napi]
    pub fn decrypt(&self, ciphertext: Buffer) -> napi::Result<Buffer> {
        let out = self.decrypt_raw(ciphertext.as_ref())?;
        Ok(Buffer::from(out))
    }

    /// Whether `data` carries the crypt magic header.
    pub fn is_encrypted_raw(data: &[u8]) -> bool {
        data.len() >= MAGIC_HEADER.len() && &data[..MAGIC_HEADER.len()] == MAGIC_HEADER
    }

    /// Whether `data` carries the crypt magic header.
    #[napi(js_name = "isEncrypted")]
    pub fn is_encrypted(data: Buffer) -> bool {
        Self::is_encrypted_raw(data.as_ref())
    }
}

// ---------------------------------------------------------------------------
// KeyManager / IdentityManager
//
// Both types are defined canonically in `src/vault.rs` (together with their
// NAPI bindings). They are re-exported here so the git-crypt subsystem in this
// module keeps resolving `KeyManager` / `IdentityManager`.
// ---------------------------------------------------------------------------

#[cfg(feature = "ssh")]
pub use crate::vault::IdentityManager;
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

#[cfg(all(feature = "multi-sig", feature = "ssh"))]
mod legacy_webcrypto {
    use super::*;

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
        pub fn from_codec(codec: Either<String, u32>) -> napi::Result<Self> {
            let c = match codec {
                Either::A(name) => multi_codec::Codec::try_from(name.as_str()).map_err(|e| {
                    napi::Error::from_reason(format!("unknown codec '{name}': {e}"))
                })?,
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
        pub fn new(bytes: Buffer, key_format: Option<String>) -> napi::Result<Self> {
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
            let encoded: multi_sig::BaseEncoded<multi_sig::Multisig> =
                multi_sig::BaseEncoded::from(self.inner.clone());
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
        pub fn signature_bytes(&self) -> napi::Result<SignatureBytes> {
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
    fn resolve_codec(codec: Either<String, u32>) -> napi::Result<multi_codec::Codec> {
        match codec {
            Either::A(name) => multi_codec::Codec::try_from(name.as_str())
                .map_err(|e| napi::Error::from_reason(format!("unknown codec '{name}': {e}"))),
            Either::B(code) => multi_codec::Codec::try_from(code)
                .map_err(|e| napi::Error::from_reason(format!("unknown codec {code}: {e}"))),
        }
    }

    #[napi]
    pub struct Signature {
        inner: multi_sig::Builder,
    }

    #[napi]
    impl Signature {
        /// Create a builder for `codec` — a multicodec name (`"eddsa-msig"`) or numeric value.
        #[napi(constructor)]
        pub fn new(codec: Either<String, u32>) -> napi::Result<Self> {
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
        pub fn with_payload_encoding(&mut self, codec: Either<String, u32>) -> napi::Result<&Self> {
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
        ) -> napi::Result<&Self> {
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

        /// Construct from a BLS12-381 signature (`rustls::Signature`) with an explicit codec —
        /// `"bls12_381-g1-msig"` (48-byte sig) or `"bls12_381-g2-msig"` (96-byte sig).
        #[napi(factory)]
        pub fn new_from_bls_signature(
            codec: Either<String, u32>,
            signature: Buffer,
        ) -> napi::Result<Self> {
            let c = resolve_codec(codec)?;
            let inner = match c {
                multi_codec::Codec::Bls12381G1Msig => {
                    let sig =
                        rustls::Signature::<rustls::Bls12381G1Impl>::try_from(signature.as_ref())
                            .map_err(|e| {
                            napi::Error::from_reason(format!("invalid BLS12-381 G1 signature: {e}"))
                        })?;
                    multi_sig::Builder::new_from_bls_signature_with_codec(c, &sig)
                        .map_err(|e| napi::Error::from_reason(format!("bls signature: {e}")))?
                }
                multi_codec::Codec::Bls12381G2Msig => {
                    let sig =
                        rustls::Signature::<rustls::Bls12381G2Impl>::try_from(signature.as_ref())
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

        /// Construct from a BLS12-381 signature *share* (`rustls::SignatureShare`) with an explicit
        /// share codec — `"bls12_381-g1-share-msig"` or `"bls12_381-g2-share-msig"`.
        #[napi(factory)]
        pub fn new_from_bls_signature_share(
            codec: Either<String, u32>,
            threshold: u32,
            limit: u32,
            share: Buffer,
        ) -> napi::Result<Self> {
            let c = resolve_codec(codec)?;
            let inner = match c {
                multi_codec::Codec::Bls12381G1ShareMsig => {
                    let s =
                        rustls::SignatureShare::<rustls::Bls12381G1Impl>::try_from(share.as_ref())
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
                    let s =
                        rustls::SignatureShare::<rustls::Bls12381G2Impl>::try_from(share.as_ref())
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
        pub fn try_build(&self) -> napi::Result<MultiSignature> {
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
        pub fn try_build_encoded(&self) -> napi::Result<String> {
            self.inner
                .clone()
                .try_build_encoded()
                .map(|encoded| encoded.to_string())
                .map_err(|e| napi::Error::from_reason(format!("build multisig: {e}")))
        }
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
    /// Active crypto mode for the repository (filter, entire_repo, or hybrid).
    #[napi]
    pub fn mode(&self) -> CryptoMode {
        let config_path = self.config_path();
        let mut has_filter = false;
        let mut has_entire_repo = false;

        if config_path.exists() {
            if let Ok(config) =
                gix::config::File::from_path_no_includes(config_path, gix::config::Source::Local)
            {
                if config.string("filter.crypto.clean").is_some()
                    || config.string("filter.git-crypt.clean").is_some()
                {
                    has_filter = true;
                }
                if let Some(val) = config.string("gcrypt.mode") {
                    if val == "entire_repo" {
                        has_entire_repo = true;
                    }
                }
            }
        }

        match (has_filter, has_entire_repo) {
            (true, true) => CryptoMode::Hybrid,
            (false, true) => CryptoMode::EntireRepo,
            _ => CryptoMode::Filter,
        }
    }

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
    pub fn generate_key(&self) -> napi::Result<SymmetricCryptoKey> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized.into());
        }
        Ok(km.generate_key()?)
    }

    /// Initialize crypt: create key directory, generate a key, set gcrypt.mode = entire_repo,
    /// and configure git-crypt filter drivers.
    /// Both full repo encryption (git-remote-gcrypt) and partial worktree encryption (git-crypt)
    /// are applied by default (Hybrid mode).
    #[napi]
    pub fn init(&self, opts: Option<CryptFilterOptions>) -> napi::Result<String> {
        let km = self.key_manager();
        if !km.is_initialized() {
            km.init_dirs()?;
            let _key = km.generate_key()?;
        }

        self.configure_filters(&opts.unwrap_or_default().resolve())?;

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

        config
            .set_raw_value("gcrypt.mode", "entire_repo")
            .map_err(|e| CryptoError::Git(e.to_string()))?;

        let existing_id = config.string("gcrypt.id").map(|s| s.to_string());
        let repo_id = match existing_id {
            Some(id) if !id.is_empty() => id,
            _ => format!(
                ":id:{}",
                hex_lower(&SymmetricCryptoKey::generate_random().as_bytes()[..15])
            ),
        };

        config
            .set_raw_value("gcrypt.id", &repo_id)
            .map_err(|e| CryptoError::Git(e.to_string()))?;

        let mut file = std::fs::File::create(&config_path)?;
        config.write_to(&mut file)?;

        Ok(repo_id)
    }

    /// Initialize the repository for Entire Repository Encryption mode (gcrypt mode).
    /// Alias for init() which sets up both full repository encryption and partial filter drivers.
    #[napi]
    pub fn init_entire_repo(&self, opts: Option<CryptFilterOptions>) -> napi::Result<String> {
        self.init(opts)
    }

    /// Whether repository is configured with Entire Repository Encryption enabled (entire_repo or hybrid).
    #[napi]
    pub fn is_entire_repo_mode(&self) -> bool {
        let m = self.mode();
        m == CryptoMode::EntireRepo || m == CryptoMode::Hybrid
    }

    /// Lock the repository by removing the `filter.<name>.*` config (encrypted files then
    /// show their encrypted content on checkout). `opts.filterName` selects which section to
    /// remove (defaults to `crypto`).
    #[napi]
    pub fn lock(&self, opts: Option<CryptFilterOptions>) -> napi::Result<()> {
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
    pub fn unlock(
        &self,
        key_file: Option<String>,
        opts: Option<CryptFilterOptions>,
    ) -> napi::Result<()> {
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
    pub fn export_key(&self, output_path: String) -> napi::Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized.into());
        }
        km.export_key(Path::new(&output_path))?;
        Ok(())
    }

    /// Import a key from `input_path` as the default key.
    #[napi]
    pub fn import_key(&self, input_path: String) -> napi::Result<()> {
        let km = self.key_manager();
        if !km.is_initialized() {
            return Err(CryptoError::NotInitialized.into());
        }
        km.import_key(Path::new(&input_path))?;
        Ok(())
    }

    /// Encrypt `data`, returning `MAGIC_HEADER || nonce || ciphertext+tag`.
    #[napi]
    pub fn encrypt(&self, data: Buffer) -> napi::Result<Buffer> {
        let key = self.load_key()?;
        let out = key.encrypt_raw(data.as_ref())?;
        Ok(Buffer::from(out))
    }

    /// Decrypt `data` produced by [`Crypto::encrypt`].
    #[napi]
    pub fn decrypt(&self, data: Buffer) -> napi::Result<Buffer> {
        let key = self.load_key()?;
        let out = key.decrypt_raw(data.as_ref())?;
        Ok(Buffer::from(out))
    }

    /// Whether `data` carries the crypt magic header.
    #[napi]
    pub fn is_encrypted(&self, data: Buffer) -> bool {
        SymmetricCryptoKey::is_encrypted_raw(data.as_ref())
    }

    /// Encrypt a repo manifest payload using the repository key.
    #[napi]
    pub fn encrypt_manifest(&self, manifest: RepoManifest) -> napi::Result<Buffer> {
        let key = self.load_key()?;
        let text = serialize_manifest(manifest);
        let enc = key.encrypt_raw(text.as_bytes())?;
        Ok(Buffer::from(enc))
    }

    /// Decrypt a repo manifest payload using the repository key.
    #[napi]
    pub fn decrypt_manifest(&self, ciphertext: Buffer) -> napi::Result<RepoManifest> {
        let key = self.load_key()?;
        let dec = key.decrypt_raw(ciphertext.as_ref())?;
        let text = String::from_utf8(dec)
            .map_err(|e| napi::Error::from_reason(format!("manifest UTF-8 error: {e}")))?;
        parse_manifest(text)
    }

    /// Encrypt a packfile buffer using a symmetric key (or a newly generated key).
    /// Returns the SHA256 digest of the encrypted output as pack_id.
    #[napi]
    pub fn encrypt_pack(
        &self,
        pack_data: Buffer,
        pack_key: Option<String>,
    ) -> napi::Result<EncryptedPackResult> {
        use sha2::{Digest, Sha256};

        let key = match pack_key {
            Some(ref k) => {
                let bytes = hex::decode(k)
                    .map_err(|e| napi::Error::from_reason(format!("invalid pack key hex: {e}")))?;
                if bytes.len() < KEY_SIZE {
                    return Err(napi::Error::from_reason(format!(
                        "pack key must be at least {KEY_SIZE} bytes"
                    )));
                }
                SymmetricCryptoKey::from_bytes(&bytes[..KEY_SIZE])?
            }
            None => SymmetricCryptoKey::generate_random(),
        };

        let encrypted = key.encrypt_raw(pack_data.as_ref())?;

        let mut hasher = Sha256::new();
        hasher.update(&encrypted);
        let digest = hasher.finalize();
        let pack_id = hex_lower(&digest);

        let key_hex = hex_lower(key.as_bytes());

        Ok(EncryptedPackResult {
            pack_id,
            hash_type: "SHA256".to_string(),
            pack_key: key_hex,
            encrypted_bytes: Buffer::from(encrypted),
        })
    }

    /// Decrypt an encrypted packfile buffer using its pack key and verify its digest.
    #[napi]
    pub fn decrypt_pack(
        &self,
        encrypted_pack_data: Buffer,
        hash_type: String,
        expected_pack_id: String,
        pack_key: String,
    ) -> napi::Result<Buffer> {
        use sha2::{Digest, Sha256};

        if hash_type != "SHA256" && hash_type != "sha256" {
            return Err(napi::Error::from_reason(format!(
                "unsupported hash type: {hash_type}"
            )));
        }

        let mut hasher = Sha256::new();
        hasher.update(encrypted_pack_data.as_ref());
        let digest = hasher.finalize();
        let calculated_id = hex_lower(&digest);

        if !calculated_id.eq_ignore_ascii_case(&expected_pack_id) {
            return Err(CryptoError::Crypto(format!(
                "packfile digest mismatch: expected {expected_pack_id}, got {calculated_id}"
            ))
            .into());
        }

        let key_bytes = hex::decode(&pack_key)
            .map_err(|e| napi::Error::from_reason(format!("invalid pack key hex: {e}")))?;
        if key_bytes.len() < KEY_SIZE {
            return Err(napi::Error::from_reason(format!(
                "pack key must be at least {KEY_SIZE} bytes"
            )));
        }

        let key = SymmetricCryptoKey::from_bytes(&key_bytes[..KEY_SIZE])?;
        let decrypted = key.decrypt_raw(encrypted_pack_data.as_ref())?;

        Ok(Buffer::from(decrypted))
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
    pub fn add_ssh_user(&self, ssh_key_path: String, alias: Option<String>) -> napi::Result<()> {
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
    ) -> napi::Result<()> {
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
    ) -> napi::Result<()> {
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
    pub fn get_public_key(&self, name: Option<String>) -> napi::Result<String> {
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
    pub fn sign(&self, data: Buffer, name: Option<String>) -> napi::Result<Buffer> {
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
    pub fn verify(
        &self,
        data: Buffer,
        signature: Buffer,
        name: Option<String>,
    ) -> napi::Result<bool> {
        let name = name.unwrap_or_else(|| "sign".to_string());
        let pub_text = self.get_public_key(Some(name))?;
        SshIdentity::verify_with_public_key(pub_text, data, signature)
    }

    /// List the distinct key names stored under `refs/keys/*` (e.g. `["sign", "alice"]`).
    #[napi]
    pub fn list_keys(&self) -> napi::Result<Vec<String>> {
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
    pub fn authenticate(
        &self,
        identity_pem: String,
        passphrase: Option<String>,
    ) -> napi::Result<bool> {
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
    pub fn generate_xmss_key(&self, name: String, passphrase: Option<String>) -> napi::Result<()> {
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
    pub fn xmss_used(&self, name: String) -> napi::Result<u32> {
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
    ) -> napi::Result<Buffer> {
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
    pub fn verify_xmss(&self, name: String, data: Buffer, signature: Buffer) -> napi::Result<bool> {
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
    pub fn add_gpg_user(gpg_id: String) -> napi::Result<()> {
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
    if SymmetricCryptoKey::is_encrypted_raw(&input) {
        io::stdout().write_all(&input)?;
        return Ok(());
    }
    let encrypted = key.encrypt_raw(&input)?;
    io::stdout().write_all(&encrypted)?;
    Ok(())
}

/// Smudge filter: decrypt stdin to stdout (pass-through if not encrypted).
#[allow(dead_code)]
pub fn smudge_filter(key: &SymmetricCryptoKey) -> CryptoResult<()> {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;
    if !SymmetricCryptoKey::is_encrypted_raw(&input) {
        io::stdout().write_all(&input)?;
        return Ok(());
    }
    let decrypted = key.decrypt_raw(&input)?;
    io::stdout().write_all(&decrypted)?;
    Ok(())
}

/// Diff filter: replace encrypted content with a placeholder notice.
#[allow(dead_code)]
pub fn diff_filter() -> CryptoResult<()> {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;
    if SymmetricCryptoKey::is_encrypted_raw(&input) {
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
// `IdentityManager` is re-exported from `crate::vault` (see the top of this
// module). The SSH key parsing/conversion helpers that follow remain defined
// here because the ref-backed key vault in this module still depends on them.

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
    pub fn from_private_key(pem: String, passphrase: Option<String>) -> napi::Result<Self> {
        let pass = passphrase.unwrap_or_default();
        let (seed, public) = parse_ed25519_private(&pem, &pass)
            .map_err(|e| napi::Error::from_reason(format!("invalid SSH identity: {e}")))?;
        Ok(Self { seed, public })
    }

    /// Ed25519-sign `data`, returning the 64-byte raw signature.
    #[napi]
    pub fn sign(&self, data: Buffer) -> napi::Result<Buffer> {
        let kp = signature::Ed25519KeyPair::from_seed_unchecked(&self.seed)
            .map_err(|_| napi::Error::from_reason("invalid ed25519 seed".to_string()))?;
        let sig = kp.sign(data.as_ref());
        Ok(Buffer::from(sig.as_ref().to_vec()))
    }

    /// Verify an Ed25519 `signature` over `data` using this identity's public key.
    #[napi]
    pub fn verify(&self, data: Buffer, signature: Buffer) -> napi::Result<bool> {
        let pk = signature::UnparsedPublicKey::new(&signature::ED25519, &self.public);
        Ok(pk.verify(data.as_ref(), signature.as_ref()).is_ok())
    }

    /// Verify an Ed25519 `signature` over `data` using a public-key PEM/string.
    #[napi]
    pub fn verify_with_public_key(
        pub_pem: String,
        data: Buffer,
        signature: Buffer,
    ) -> napi::Result<bool> {
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
    pub fn encrypt_key_for_recipient(
        _key: &SymmetricCryptoKey,
        _recipient: &str,
    ) -> CryptoResult<Vec<u8>> {
        Err(CryptoError::Gpg(
            "GPG recipient encryption is not yet implemented (keyring lookup pending)".into(),
        ))
    }
}

#[cfg(feature = "btls")]
mod legacy_webcrypto_key {
    use super::*;
    use napi::bindgen_prelude::{ClassInstance, Uint8Array};
    use napi_derive::napi;

    fn bad(msg: impl Into<String>) -> napi::Error {
        napi::Error::new(napi::Status::InvalidArg, msg.into())
    }

    #[allow(dead_code)]
    fn fail(msg: impl Into<String>) -> napi::Error {
        napi::Error::new(napi::Status::GenericFailure, msg.into())
    }

    /// Placeholder error: the btls-backed WebCrypto implementation is not yet wired up.
    fn not_implemented() -> napi::Error {
        napi::Error::new(
            napi::Status::GenericFailure,
            "btls-backed WebCrypto is not yet implemented",
        )
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

    /// Parameters of a crypto operation (`encrypt`, `decrypt`, `sign`, `verify`,
    /// `deriveBits`, `deriveKey`, `wrapKey`, `digest`).
    #[napi(object)]
    pub struct CryptoOpParams<'a> {
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
        pub public: Option<ClassInstance<'a, CryptoKey>>,
        pub length: Option<u32>,
    }

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

    fn hash_name(hash: &Option<KeyAlgorithm>) -> String {
        hash.as_ref()
            .map(|h| h.name.clone())
            .unwrap_or_else(|| "SHA-256".to_string())
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

    fn validate_usages(kind: AlgKind, alg_name: &str, usages: &[String]) -> napi::Result<()> {
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

    /// Key material backing a [`CryptoKey`].
    ///
    /// The btls backend is not yet wired up, so asymmetric material is stored as
    /// raw bytes; only symmetric (`Secret`) keys carry usable data for now.
    #[derive(Clone)]
    enum KeyMaterial {
        Secret(Vec<u8>),
        Private(Vec<u8>),
        Public(Vec<u8>),
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

    /// The two halves of an asymmetric [`CryptoKey`].
    #[napi]
    pub struct CryptoKeyPair {
        public_key: CryptoKey,
        private_key: CryptoKey,
    }

    #[napi]
    impl CryptoKey {
        /// Generate a new key (or key pair) for `algorithm`.
        ///
        /// The btls backend is not yet implemented; this returns `NotImplemented`.
        #[napi(constructor)]
        pub fn new(
            _algorithm: Either<String, CryptoKeyAlgorithm>,
            _extractable: bool,
            _usages: Vec<String>,
        ) -> napi::Result<Self> {
            Err(not_implemented())
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
            self.public_key.clone().map(|b| *b)
        }

        /// The private half of an asymmetric key pair, else `undefined`.
        #[napi(getter, js_name = "privateKey")]
        pub fn get_private_key_prop(&self) -> Option<CryptoKey> {
            self.private_key.clone().map(|b| *b)
        }

        fn check_usage(&self, usage: &str) -> napi::Result<()> {
            if self.usages.iter().any(|u| u == usage) {
                Ok(())
            } else {
                Err(bad(format!(
                    "The requested operation is not valid for this key; '{usage}' is not in its usages"
                )))
            }
        }

        /// Import key material in `format` (`raw`, `raw-secret`, `pkcs8`, `spki`,
        /// or `jwk`).
        #[napi]
        pub fn import_key(
            _format: String,
            _key_data: Uint8Array,
            _algorithm: Either<String, CryptoKeyAlgorithm>,
            _extractable: bool,
            _usages: Vec<String>,
        ) -> napi::Result<CryptoKey> {
            Err(not_implemented())
        }

        /// Export a key. `jwk` returns the JWK object; every other format returns a
        /// `Buffer` of DER/raw bytes.
        #[napi]
        pub fn export_key(_format: String, _key: &CryptoKey) -> napi::Result<Buffer> {
            Err(not_implemented())
        }

        /// Extract the public half of an asymmetric key.
        #[napi]
        pub fn get_public_key(_key: &CryptoKey) -> napi::Result<CryptoKey> {
            Err(not_implemented())
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
                "sign" | "verify" => matches!(
                    kind,
                    AlgKind::Mac
                        | AlgKind::Ecdsa
                        | AlgKind::Ed25519
                        | AlgKind::RsaPss
                        | AlgKind::RsaPkcs1
                ),
                "digest" => true,
                "deriveBits" | "deriveKey" => {
                    matches!(kind, AlgKind::Kdf | AlgKind::Ecdh | AlgKind::X25519)
                }
                "wrapKey" | "unwrapKey" => matches!(kind, AlgKind::Aes | AlgKind::RsaOaep),
                "getPublicKey" => !is_symmetric(kind) && !matches!(kind, AlgKind::Kdf),
                _ => false,
            }
        }

        // --- operations (static) ---

        #[napi(js_name = "encrypt")]
        pub fn encrypt_static(
            _algorithm: CryptoOpParams,
            _key: &CryptoKey,
            _data: Uint8Array,
        ) -> napi::Result<Buffer> {
            Err(not_implemented())
        }

        #[napi(js_name = "decrypt")]
        pub fn decrypt_static(
            _algorithm: CryptoOpParams,
            _key: &CryptoKey,
            _data: Uint8Array,
        ) -> napi::Result<Buffer> {
            Err(not_implemented())
        }

        #[napi(js_name = "sign")]
        pub fn sign_static(
            _algorithm: CryptoOpParams,
            _key: &CryptoKey,
            _data: Uint8Array,
        ) -> napi::Result<Buffer> {
            Err(not_implemented())
        }

        #[napi(js_name = "verify")]
        pub fn verify_static(
            _algorithm: CryptoOpParams,
            _key: &CryptoKey,
            _signature: Uint8Array,
            _data: Uint8Array,
        ) -> napi::Result<bool> {
            Err(not_implemented())
        }

        #[napi(js_name = "deriveBits")]
        pub fn derive_bits_static(
            _algorithm: CryptoOpParams,
            _base_key: &CryptoKey,
            _length: Option<u32>,
        ) -> napi::Result<Buffer> {
            Err(not_implemented())
        }

        #[napi(js_name = "deriveKey")]
        pub fn derive_key_static(
            _algorithm: CryptoOpParams,
            _base_key: &CryptoKey,
            _derived_key_type: CryptoOpParams,
            _extractable: bool,
            _key_usages: Vec<String>,
        ) -> napi::Result<CryptoKey> {
            Err(not_implemented())
        }

        #[napi(js_name = "digest")]
        pub fn digest_static(
            _algorithm: CryptoOpParams,
            _data: Uint8Array,
        ) -> napi::Result<Buffer> {
            Err(not_implemented())
        }

        #[napi(js_name = "wrapKey")]
        pub fn wrap_key_static(
            _format: String,
            _key: &CryptoKey,
            _wrapping_key: &CryptoKey,
            _algorithm: Either<String, CryptoOpParams>,
        ) -> napi::Result<Buffer> {
            Err(not_implemented())
        }

        #[napi(js_name = "unwrapKey")]
        pub fn unwrap_key_static(
            _format: String,
            _wrapped_key: Uint8Array,
            _unwrapping_key: &CryptoKey,
            _wrap_algorithm: Either<String, CryptoOpParams>,
            _derived_key_algorithm: CryptoKeyAlgorithm,
            _extractable: bool,
            _key_usages: Vec<String>,
        ) -> napi::Result<CryptoKey> {
            Err(not_implemented())
        }

        // --- operations (instance) ---

        #[napi]
        pub fn encrypt(
            &self,
            algorithm: CryptoOpParams,
            key: &CryptoKey,
            data: Uint8Array,
        ) -> napi::Result<Buffer> {
            Self::encrypt_static(algorithm, key, data)
        }

        #[napi]
        pub fn decrypt(
            &self,
            algorithm: CryptoOpParams,
            key: &CryptoKey,
            data: Uint8Array,
        ) -> napi::Result<Buffer> {
            Self::decrypt_static(algorithm, key, data)
        }

        #[napi]
        pub fn sign(
            &self,
            algorithm: CryptoOpParams,
            key: &CryptoKey,
            data: Uint8Array,
        ) -> napi::Result<Buffer> {
            Self::sign_static(algorithm, key, data)
        }

        #[napi]
        pub fn verify(
            &self,
            algorithm: CryptoOpParams,
            key: &CryptoKey,
            signature: Uint8Array,
            data: Uint8Array,
        ) -> napi::Result<bool> {
            Self::verify_static(algorithm, key, signature, data)
        }

        #[napi]
        pub fn derive_bits(
            &self,
            algorithm: CryptoOpParams,
            base_key: &CryptoKey,
            length: Option<u32>,
        ) -> napi::Result<Buffer> {
            Self::derive_bits_static(algorithm, base_key, length)
        }

        #[napi]
        pub fn export_key_instance(&self, format: String) -> napi::Result<Buffer> {
            Self::export_key(format, self)
        }

        #[napi]
        pub fn get_public_key_instance(&self) -> napi::Result<CryptoKey> {
            Self::get_public_key(self)
        }

        #[napi]
        pub fn derive_key(
            &self,
            algorithm: CryptoOpParams,
            base_key: &CryptoKey,
            derived_key_type: CryptoOpParams,
            extractable: bool,
            key_usages: Vec<String>,
        ) -> napi::Result<CryptoKey> {
            Self::derive_key_static(
                algorithm,
                base_key,
                derived_key_type,
                extractable,
                key_usages,
            )
        }

        #[napi]
        pub fn wrap_key(
            &self,
            format: String,
            key: &CryptoKey,
            wrapping_key: &CryptoKey,
            algorithm: Either<String, CryptoOpParams>,
        ) -> napi::Result<Buffer> {
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
        ) -> napi::Result<CryptoKey> {
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
        pub fn digest(&self, algorithm: CryptoOpParams, data: Uint8Array) -> napi::Result<Buffer> {
            Self::digest_static(algorithm, data)
        }

        // Post-quantum KEMs are out of scope for this build; the surface is present
        // so callers can feature-detect and fail with a clear error.

        #[napi(js_name = "encapsulateBits")]
        pub fn encapsulate_bits_static(
            _algorithm: CryptoOpParams,
            _public_key: &CryptoKey,
        ) -> napi::Result<Buffer> {
            Err(bad(
                "encapsulateBits (ML-KEM) is not implemented in this build",
            ))
        }

        #[napi(js_name = "decapsulateBits")]
        pub fn decapsulate_bits_static(
            _algorithm: CryptoOpParams,
            _private_key: &CryptoKey,
            _ciphertext: Uint8Array,
        ) -> napi::Result<Buffer> {
            Err(bad(
                "decapsulateBits (ML-KEM) is not implemented in this build",
            ))
        }

        #[napi(js_name = "encapsulateKey")]
        pub fn encapsulate_key_static(
            _algorithm: CryptoOpParams,
            _public_key: &CryptoKey,
            _derived_key_type: CryptoOpParams,
            _extractable: bool,
            _key_usages: Vec<String>,
        ) -> napi::Result<CryptoKey> {
            Err(bad(
                "encapsulateKey (ML-KEM) is not implemented in this build",
            ))
        }

        #[napi(js_name = "decapsulateKey")]
        pub fn decapsulate_key_static(
            _algorithm: CryptoOpParams,
            _private_key: &CryptoKey,
            _ciphertext: Uint8Array,
            _derived_key_type: CryptoOpParams,
            _extractable: bool,
            _key_usages: Vec<String>,
        ) -> napi::Result<CryptoKey> {
            Err(bad(
                "decapsulateKey (ML-KEM) is not implemented in this build",
            ))
        }

        #[napi(js_name = "encapsulateBits")]
        pub fn encapsulate_bits(
            &self,
            algorithm: CryptoOpParams,
            public_key: &CryptoKey,
        ) -> napi::Result<Buffer> {
            Self::encapsulate_bits_static(algorithm, public_key)
        }

        #[napi(js_name = "decapsulateBits")]
        pub fn decapsulate_bits(
            &self,
            algorithm: CryptoOpParams,
            private_key: &CryptoKey,
            ciphertext: Uint8Array,
        ) -> napi::Result<Buffer> {
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
        ) -> napi::Result<CryptoKey> {
            Self::encapsulate_key_static(
                algorithm,
                public_key,
                derived_key_type,
                extractable,
                key_usages,
            )
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
        ) -> napi::Result<CryptoKey> {
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

    /// A format-tagged cryptographic signature value.
    ///
    /// Construct it with the key format the signature corresponds to, then load the
    /// raw signature bytes.
    #[napi]
    pub struct SignatureValue {
        format: String,
        bytes: Vec<u8>,
    }

    #[napi]
    impl SignatureValue {
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
}

#[cfg(all(test, feature = "btls"))]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn deterministic_key(byte: u8) -> CryptoKey {
        CryptoKey::from_bytes(&vec![byte; KEY_SIZE]).unwrap()
    }

    // ---- CryptoKey ----

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = CryptoKey::generate();
        let plaintext = b"Hello, World!";
        let ciphertext = key.encrypt(plaintext).unwrap();
        assert_ne!(plaintext.as_slice(), &ciphertext[..]);
        let decrypted = key.decrypt(&ciphertext).unwrap();
        assert_eq!(plaintext.as_slice(), &decrypted[..]);
    }

    #[test]
    fn encrypt_is_randomized_and_detected() {
        let key = CryptoKey::generate();
        let c1 = key.encrypt(b"same").unwrap();
        let c2 = key.encrypt(b"same").unwrap();
        assert_ne!(c1, c2, "fresh nonce per call");
        assert!(CryptoKey::is_encrypted(&c1));
        assert!(!CryptoKey::is_encrypted(b"plain"));
    }

    #[test]
    fn wrong_key_fails_and_tamper_detected() {
        let key1 = CryptoKey::generate();
        let key2 = CryptoKey::generate();
        let ct = key1.encrypt(b"secret").unwrap();
        assert!(key2.decrypt(&ct).is_err());

        let mut tampered = ct.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xFF;
        assert!(key1.decrypt(&tampered).is_err());
    }

    #[test]
    fn empty_and_invalid_format() {
        let key = CryptoKey::generate();
        let ct = key.encrypt(b"").unwrap();
        assert_eq!(key.decrypt(&ct).unwrap(), b"".to_vec());
        assert!(key.decrypt(&[1, 2, 3]).is_err());
        assert!(CryptoKey::from_bytes(&[0u8; KEY_SIZE - 1]).is_err());
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

// ---------------------------------------------------------------------------
// Git filter and path configuration functions
// ---------------------------------------------------------------------------

pub fn configure_git_filters(key_name: Option<&str>) -> CryptoResult<()> {
    let exe_path = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "git-crypt".to_string());
    let escaped_path = format!("\"{exe_path}\"");

    let repo = gix::open(".").map_err(|e| CryptoError::Git(e.to_string()))?;
    let config_path = repo.git_dir().join("config");
    let mut config = if config_path.exists() {
        gix::config::File::from_path_no_includes(config_path.clone(), gix::config::Source::Local)
            .map_err(|e| CryptoError::Git(e.to_string()))?
    } else {
        gix::config::File::new(gix::config::file::Metadata::from(
            gix::config::Source::Local,
        ))
    };

    let attr_name = attribute_name(key_name);

    let smudge_cmd = match key_name {
        Some(k) => format!("{escaped_path} smudge --key-name={k}"),
        None => format!("{escaped_path} smudge"),
    };
    let clean_cmd = match key_name {
        Some(k) => format!("{escaped_path} clean --key-name={k}"),
        None => format!("{escaped_path} clean"),
    };
    let diff_cmd = match key_name {
        Some(k) => format!("{escaped_path} diff --key-name={k}"),
        None => format!("{escaped_path} diff"),
    };

    config
        .set_raw_value(&format!("filter.{attr_name}.smudge"), &smudge_cmd)
        .map_err(|e| CryptoError::Git(e.to_string()))?;
    config
        .set_raw_value(&format!("filter.{attr_name}.clean"), &clean_cmd)
        .map_err(|e| CryptoError::Git(e.to_string()))?;
    config
        .set_raw_value(&format!("filter.{attr_name}.required"), "true")
        .map_err(|e| CryptoError::Git(e.to_string()))?;
    config
        .set_raw_value(&format!("diff.{attr_name}.textconv"), &diff_cmd)
        .map_err(|e| CryptoError::Git(e.to_string()))?;

    let mut file = std::fs::File::create(&config_path)?;
    config.write_to(&mut file)?;
    Ok(())
}

pub fn deconfigure_git_filters(key_name: Option<&str>) -> CryptoResult<()> {
    let repo = gix::open(".").map_err(|e| CryptoError::Git(e.to_string()))?;
    let config_path = repo.git_dir().join("config");
    if !config_path.exists() {
        return Ok(());
    }
    let mut config =
        gix::config::File::from_path_no_includes(config_path.clone(), gix::config::Source::Local)
            .map_err(|e| CryptoError::Git(e.to_string()))?;

    let attr_name = attribute_name(key_name);

    let _ = config.remove_section("filter", Some(attr_name.as_str().into()));
    let _ = config.remove_section("diff", Some(attr_name.as_str().into()));

    let mut file = std::fs::File::create(&config_path)?;
    config.write_to(&mut file)?;
    Ok(())
}

pub fn git_checkout_batch(paths: &[String]) -> bool {
    if paths.is_empty() {
        return true;
    }
    let mut cmd = std::process::Command::new("git");
    cmd.arg("checkout").arg("--");
    for path in paths {
        cmd.arg(path);
    }
    match cmd.status() {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}

pub fn git_checkout(paths: &[String]) -> bool {
    for chunk in paths.chunks(GIT_CHECKOUT_BATCH_SIZE) {
        if !git_checkout_batch(chunk) {
            return false;
        }
    }
    true
}

pub fn validate_key_name(key_name: Option<&str>) -> std::result::Result<(), String> {
    if let Some(name) = key_name {
        if name.is_empty() {
            return Err("Key name cannot be empty".to_string());
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
        {
            return Err("Key name contains invalid characters".to_string());
        }
    }
    Ok(())
}

pub fn validate_key_name_or_throw(key_name: Option<&str>) -> CryptoResult<()> {
    validate_key_name(key_name).map_err(CryptoError::Crypto)
}

pub fn get_git_config(name: &str) -> CryptoResult<String> {
    let repo = gix::open(".").map_err(|e| CryptoError::Git(e.to_string()))?;
    let val = repo
        .config_snapshot()
        .string(name)
        .ok_or_else(|| CryptoError::Git(format!("'git config' missing value for key '{name}'")))?;
    Ok(val.to_string())
}

pub fn get_internal_state_path() -> CryptoResult<PathBuf> {
    let repo = gix::open(".").map_err(|e| CryptoError::Git(e.to_string()))?;
    Ok(repo.git_dir().join("git-crypt"))
}

pub fn get_internal_keys_path() -> CryptoResult<PathBuf> {
    Ok(get_internal_state_path()?.join("keys"))
}

pub fn get_internal_key_path(key_name: Option<&str>) -> CryptoResult<PathBuf> {
    let keys_dir = get_internal_keys_path()?;
    let name = key_name.unwrap_or("default");
    Ok(keys_dir.join(name))
}

pub fn get_repo_state_path() -> CryptoResult<PathBuf> {
    let repo = gix::open(".").map_err(|e| CryptoError::Git(e.to_string()))?;
    let top = repo
        .workdir()
        .ok_or_else(|| CryptoError::Git("Could not determine Git working tree".to_string()))?;

    if let Ok(rel) = get_git_config("git-crypt.repoStateDir") {
        Ok(top.join(rel))
    } else {
        Ok(top.join(".git-crypt"))
    }
}

pub fn get_repo_keys_path() -> CryptoResult<PathBuf> {
    Ok(get_repo_state_path()?.join("keys"))
}

pub fn get_path_to_top() -> CryptoResult<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-cdup"])
        .output()
        .map_err(|e| CryptoError::Git(e.to_string()))?;
    if !output.status.success() {
        return Err(CryptoError::Git(
            "'git rev-parse --show-cdup' failed".to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn get_git_status() -> CryptoResult<String> {
    let output = std::process::Command::new("git")
        .args(["status", "-uno", "--porcelain"])
        .output()
        .map_err(|e| CryptoError::Git(e.to_string()))?;
    if !output.status.success() {
        return Err(CryptoError::Git("'git status' failed".to_string()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn get_file_attributes(filename: &str) -> CryptoResult<(String, String)> {
    let output = std::process::Command::new("git")
        .args(["check-attr", "filter", "diff", "--", filename])
        .output()
        .map_err(|e| CryptoError::Git(e.to_string()))?;
    if !output.status.success() {
        return Err(CryptoError::Git("'git check-attr' failed".to_string()));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut filter_attr = String::new();
    let mut diff_attr = String::new();

    for line in stdout.lines() {
        if let Some(value_pos) = line.rfind(": ") {
            if value_pos == 0 {
                continue;
            }
            if let Some(name_pos) = line[..value_pos].rfind(": ") {
                let attr_name = &line[name_pos + 2..value_pos];
                let attr_value = &line[value_pos + 2..];
                if attr_value != "unspecified" && attr_value != "unset" && attr_value != "set" {
                    if attr_name == "filter" {
                        filter_attr = attr_value.to_string();
                    } else if attr_name == "diff" {
                        diff_attr = attr_value.to_string();
                    }
                }
            }
        }
    }
    Ok((filter_attr, diff_attr))
}

pub fn check_if_blob_is_encrypted(object_id: &str) -> CryptoResult<bool> {
    let output = std::process::Command::new("git")
        .args(["cat-file", "blob", object_id])
        .output()
        .map_err(|e| CryptoError::Git(e.to_string()))?;
    if !output.status.success() {
        return Err(CryptoError::Git("'git cat-file' failed".to_string()));
    }
    Ok(SymmetricCryptoKey::is_encrypted_raw(&output.stdout))
}

pub fn check_if_file_is_encrypted(filename: &str) -> CryptoResult<bool> {
    let output = std::process::Command::new("git")
        .args(["ls-files", "-sz", "--", filename])
        .output()
        .map_err(|e| CryptoError::Git(e.to_string()))?;
    if !output.status.success() {
        return Err(CryptoError::Git("'git ls-files' failed".to_string()));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = stdout.split_whitespace().collect();
    if parts.len() >= 2 {
        let object_id = parts[1];
        return check_if_blob_is_encrypted(object_id);
    }
    Ok(false)
}

pub fn attribute_name(key_name: Option<&str>) -> String {
    match key_name {
        Some(k) => format!("git-crypt-{k}"),
        None => "git-crypt".to_string(),
    }
}

pub fn get_encrypted_files(files: &mut Vec<String>, key_name: Option<&str>) -> CryptoResult<()> {
    let target_attr = attribute_name(key_name);
    let path_to_top = get_path_to_top().unwrap_or_default();
    let mut args = vec!["ls-files", "-csz", "--"];
    if !path_to_top.is_empty() {
        args.push(&path_to_top);
    }
    let output = std::process::Command::new("git")
        .args(&args)
        .output()
        .map_err(|e| CryptoError::Git(e.to_string()))?;
    if !output.status.success() {
        return Err(CryptoError::Git("'git ls-files' failed".to_string()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for entry in stdout.split('\0') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let parts: Vec<&str> = entry.split_whitespace().collect();
        if parts.len() >= 4 {
            let filename = parts[3];
            if let Ok((filter_attr, _)) = get_file_attributes(filename) {
                if filter_attr == target_attr {
                    files.push(filename.to_string());
                }
            }
        }
    }
    Ok(())
}

pub fn decrypt_repo_key(
    key_name: Option<&str>,
    key_version: u32,
    secret_keys: &[String],
    keys_path: &Path,
) -> CryptoResult<Vec<u8>> {
    let key_dir_name = key_name.unwrap_or("default");
    for seckey in secret_keys {
        let file_path = keys_path
            .join(key_dir_name)
            .join(key_version.to_string())
            .join(format!("{seckey}.gpg"));
        if file_path.exists() {
            if let Ok(decrypted) =
                crate::gpg::gpg_decrypt_from_file(file_path.to_string_lossy().into_owned())
            {
                return Ok(decrypted.to_vec());
            }
        }
    }
    Err(CryptoError::Gpg(
        "Failed to decrypt repo key with GPG".to_string(),
    ))
}

pub fn encrypt_repo_key(
    key_name: Option<&str>,
    key_bytes: &[u8],
    collab_keys: &[(String, bool)],
    keys_path: &Path,
    key_version: u32,
) -> CryptoResult<Vec<String>> {
    let mut new_files = Vec::new();
    let key_dir_name = key_name.unwrap_or("default");

    for (fingerprint, key_is_trusted) in collab_keys {
        let file_path = keys_path
            .join(key_dir_name)
            .join(key_version.to_string())
            .join(format!("{fingerprint}.gpg"));

        if file_path.exists() {
            continue;
        }

        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        crate::gpg::gpg_encrypt_to_file(
            file_path.to_string_lossy().into_owned(),
            fingerprint.clone(),
            *key_is_trusted,
            Buffer::from(key_bytes.to_vec()),
        )
        .map_err(|e| CryptoError::Gpg(e.to_string()))?;

        new_files.push(file_path.to_string_lossy().into_owned());
    }

    Ok(new_files)
}

pub const GIT_CHECKOUT_BATCH_SIZE: usize = 500;
